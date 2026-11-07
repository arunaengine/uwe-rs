//! Resolves local Conda environment files into inspected images before task cache lookup.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::sync::Arc;
use std::time::Duration;

use content_store::{Holder, Store};
use executor::conda::{BuildLimits, TransferLimits};
use executor::conda_cache::{CacheContext, CacheLimits, ensure_with};
use executor::{Backend, CancellationToken, ExecutorError, Finished, Job, LocalContainer};
use tokio::sync::Semaphore;

use crate::conda::Config;

pub struct Local {
    pub backend: LocalContainer,
    pub store: Store,
    pub config: Option<Config>,
    pub holder: Holder,
    pub preparing: Arc<Semaphore>,
    pub secrets: std::collections::BTreeMap<String, config_file::Secret>,
    /// Where the values of the variables that a job names come from.
    pub variables: config_file::SecretValues,
}

impl Local {
    async fn prepare(
        &self,
        image: &str,
        registry: Option<&str>,
    ) -> Result<Option<String>, ExecutorError> {
        let (base, mut source) = if let Some(value) = image.strip_prefix("conda-container:") {
            let pair: [String; 2] = serde_json::from_str(value).map_err(|error| {
                ExecutorError::Unsupported(format!("Invalid Conda container reference: {error}"))
            })?;
            (Some(pair[0].clone()), pair[1].clone())
        } else if let Some(source) = image.strip_prefix("conda-env:") {
            (None, source.to_owned())
        } else {
            return self.backend.pin(image).await;
        };
        let inline = source
            .strip_prefix("nextflow-packages:")
            .map(crate::nextflow::conda_file)
            .transpose()?;
        if let Some(file) = &inline {
            source = file.path().to_string_lossy().into_owned();
        }
        if let Some(base) = &base
            && self.config.is_none()
        {
            return self.backend.pin(base).await;
        }
        let config = self.config.clone().ok_or_else(|| ExecutorError::Unsupported(
            "Conda rules require explicit [conda] configuration with a pinned micromamba base, target capabilities, repodata and packages".into(),
        ))?;
        let base = if let Some(base) = base {
            let pinned = self.backend.pin(&base).await?.ok_or_else(|| {
                ExecutorError::Unsupported(
                    "Conda container base must have a manifest digest".into(),
                )
            })?;
            if pinned.rsplit_once("@sha256:").map(|(_, digest)| digest)
                != config
                    .base
                    .rsplit_once("@sha256:")
                    .map(|(_, digest)| digest)
            {
                return Err(ExecutorError::Unsupported("Conda rule container must match [conda].base, whose target capabilities describe that exact image".into()));
            }
            Some(pinned)
        } else {
            None
        };
        let cancel = CancellationToken::new();
        let _cancel_on_drop = cancel.clone().drop_guard();
        let failed =
            |error: String| ExecutorError::Unsupported(format!("Conda image preparation: {error}"));
        let work = async {
            let permit = Arc::new(
                self.preparing
                    .clone()
                    .acquire_owned()
                    .await
                    .map_err(|error| failed(error.to_string()))?,
            );
            let packages = config.packages.clone();
            let acquisition = config.acquisition.clone();
            let preparation = tokio::time::timeout(Duration::from_secs(300), async {
                if config.offline {
                    let retained = permit.clone();
                    let local = config.clone();
                    let source = source.clone();
                    let prepared = tokio::task::spawn_blocking(move || {
                        let _permit = retained;
                        local.preparation(&source)
                    })
                    .await
                    .map_err(|error| failed(error.to_string()))?;
                    match prepared {
                        Ok(preparation) => return Ok(preparation),
                        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                            return Err(failed(error.to_string()));
                        }
                        Err(_) => {}
                    }
                }
                config
                    .acquire_preparation(&source)
                    .await
                    .map_err(|error| failed(error.to_string()))
            })
            .await
            .map_err(|_| failed("source preparation exceeded 300 seconds".into()))??;
            let preparation = if let Some(base) = &base {
                conda_environment::ImagePreparation::with_installer(
                    preparation.lock(),
                    base,
                    &config.platform,
                    &config.capabilities,
                )
                .map_err(|error| failed(error.to_string()))?
            } else {
                preparation
            };
            let limits = CacheLimits {
                build: BuildLimits {
                    context_bytes: 256 << 20,
                    progress_bytes: 8 << 20,
                    timeout: Duration::from_secs(600),
                    memory_bytes: 1 << 30,
                    cpu_quota: 100_000,
                },
                transfer: TransferLimits {
                    archive_bytes: 4 << 30,
                    progress_bytes: 8 << 20,
                    timeout: Duration::from_secs(600),
                },
                timeout: Duration::from_secs(900),
                attempts: 300,
                retry_delay: Duration::from_secs(1),
                candidates: 64,
            };
            let engine = &self.backend.engine;
            let receipt = ensure_with(
                engine,
                &preparation,
                &packages,
                CacheContext {
                    store: &self.store,
                    holder: &self.holder,
                    registry,
                },
                limits,
                &cancel,
                || async {
                    executor::conda::require_base(engine, &preparation).await?;
                    acquisition
                        .fetch_packages(preparation.lock(), &packages)
                        .await?;
                    Ok(())
                },
            )
            .await
            .map_err(|error| failed(error.to_string()))?;
            drop(permit);
            let reference = if registry.is_some() {
                receipt
                    .reference()
                    .ok_or_else(|| failed("published image has no registry reference".into()))?
            } else {
                receipt.image_id()
            };
            Ok(Some(reference.to_owned()))
        };
        tokio::time::timeout(Duration::from_secs(1200), work)
            .await
            .map_err(|_| failed("preparation exceeded 1200 seconds".into()))?
    }
}

/// The spec of a task with the values of the variables that it names in
/// `snakemake_runtime::ENVIRONMENT_NAMES`, read from the values of the run as the job starts,
/// so that only the names are part of its records and its key.
fn with_environment(
    spec: &task_model::TaskSpec,
    variables: &config_file::SecretValues,
) -> Result<Option<task_model::TaskSpec>, ExecutorError> {
    let Some(task_model::EnvironmentValue::Literal(names)) =
        spec.environment.get(snakemake_runtime::ENVIRONMENT_NAMES)
    else {
        return Ok(None);
    };
    let failed = ExecutorError::Unsupported;
    let names: Vec<String> = serde_json::from_str(names)
        .map_err(|_| failed("Invalid names of environment variables".into()))?;
    let mut spec = spec.clone();
    spec.environment
        .remove(snakemake_runtime::ENVIRONMENT_NAMES);
    for name in names {
        let value = variables.variable(&name).ok_or_else(|| {
            failed(format!(
                "The environment variable {name} that the workflow requests is not set"
            ))
        })?;
        (spec.environment).insert(name, task_model::EnvironmentValue::Literal(value));
    }
    Ok(Some(spec))
}

impl Backend for Local {
    async fn pin(&self, image: &str) -> Result<Option<String>, ExecutorError> {
        self.prepare(image, None).await
    }

    async fn run(
        &self,
        job: Job<'_>,
        cancel: &CancellationToken,
    ) -> Result<Finished, ExecutorError> {
        let filled = with_environment(job.spec, &self.variables)?;
        let job = Job {
            spec: filled.as_ref().unwrap_or(job.spec),
            ..job
        };
        let Some(task_model::EnvironmentValue::Literal(names)) = job
            .spec
            .environment
            .get(nextflow_runtime::task::SECRET_NAMES)
        else {
            return self.backend.run(job, cancel).await;
        };
        let file = crate::nextflow::secret_file(names, &self.secrets)?;
        let mut spec = job.spec.clone();
        spec.environment
            .remove(nextflow_runtime::task::SECRET_NAMES);
        let mut staging = job.staging.clone();
        staging.inputs.push(executor::Staged {
            path: nextflow_runtime::task::SECRET_FILE.into(),
            source: file.path().to_owned(),
            ..Default::default()
        });
        self.backend
            .run(
                Job {
                    spec: &spec,
                    staging: &staging,
                    ..job
                },
                cancel,
            )
            .await
    }

    fn hold(&self) {
        self.backend.hold();
    }

    async fn release(&self) {
        self.backend.release().await;
    }
}

pub struct Remote<B> {
    backend: B,
    config: Option<Config>,
    engine: String,
    store: Store,
    scratch: std::path::PathBuf,
    holder: Holder,
    local: tokio::sync::OnceCell<Local>,
    variables: config_file::SecretValues,
}

impl<B> Remote<B> {
    pub fn new(
        backend: B,
        store: Store,
        scratch: &std::path::Path,
        run: &str,
        setup: &crate::setup::Setup,
    ) -> Self {
        Self {
            backend,
            config: setup.conda.clone(),
            engine: setup.engine(),
            store,
            scratch: scratch.to_owned(),
            holder: Holder::new(run, "Conda image publication"),
            local: Default::default(),
            variables: setup.variables.clone(),
        }
    }
}

impl<B: Backend> Backend for Remote<B> {
    async fn pin(&self, image: &str) -> Result<Option<String>, ExecutorError> {
        if !task_model::conda_image(image) {
            return self.backend.pin(image).await;
        }
        let registry = self.config.as_ref().and_then(|config| config.registry.as_deref()).ok_or_else(|| {
            ExecutorError::Unsupported("Remote Conda tasks require [conda].registry = \"host/team/image\" and explicit Conda preparation settings".into())
        })?;
        let local = self
            .local
            .get_or_try_init(|| async {
                let engine = executor::ContainerEngine::named(
                    Some(self.engine.as_str()).filter(|name| !name.is_empty()),
                )
                .await
                .map_err(|error| {
                    ExecutorError::Unsupported(format!(
                        "Conda publication needs a local container engine: {error}"
                    ))
                })?;
                Ok::<_, ExecutorError>(Local {
                    backend: LocalContainer::new(
                        engine,
                        self.scratch.clone(),
                        executor::Pool::of_this_machine(),
                    ),
                    store: self.store.clone(),
                    config: self.config.clone(),
                    holder: self.holder.clone(),
                    preparing: Arc::new(Semaphore::new(1)),
                    secrets: Default::default(),
                    variables: Default::default(),
                })
            })
            .await?;
        local.prepare(image, Some(registry)).await
    }

    async fn run(
        &self,
        job: Job<'_>,
        cancel: &CancellationToken,
    ) -> Result<Finished, ExecutorError> {
        let filled = with_environment(job.spec, &self.variables)?;
        let job = Job {
            spec: filled.as_ref().unwrap_or(job.spec),
            ..job
        };
        self.backend.run(job, cancel).await
    }

    fn hold(&self) {
        self.backend.hold();
    }

    async fn release(&self) {
        self.backend.release().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The values of the named variables reach the spec of the job only, from this
    /// environment, and a variable that is not set fails the job.
    #[test]
    fn fills_named_variables() {
        let mut spec = task_model::TaskSpec {
            output_manifest: None,
            format: task_model::FORMAT,
            namespace: String::new(),
            image: None,
            arguments: Vec::new(),
            workdir: None,
            environment: Default::default(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            stdin: None,
            containers: Vec::new(),
            replaces_entrypoint: false,
            run_as: None,
        };
        let process = config_file::SecretValues::default();
        assert!(with_environment(&spec, &process).unwrap().is_none());
        let names = task_model::EnvironmentValue::Literal(r#"["HOME"]"#.into());
        let key = snakemake_runtime::ENVIRONMENT_NAMES.to_owned();
        spec.environment.insert(key.clone(), names);
        let filled = with_environment(&spec, &process)
            .unwrap()
            .expect("a filled spec");
        let home = task_model::EnvironmentValue::Literal(std::env::var("HOME").unwrap());
        assert_eq!(filled.environment.get("HOME"), Some(&home));
        assert!(!filled.environment.contains_key(&key));
        // A run with values of its own reads neither more nor other values of the process.
        let mut given = config_file::SecretValues::default();
        given.allow_environment(false);
        let error = with_environment(&spec, &given)
            .err()
            .map(|error| error.to_string());
        assert!(error.is_some_and(|error| error.contains("HOME that the workflow requests")));
        given.insert("HOME", "/run/home").unwrap();
        let filled = with_environment(&spec, &given)
            .unwrap()
            .expect("a filled spec");
        let home = task_model::EnvironmentValue::Literal("/run/home".into());
        assert_eq!(filled.environment.get("HOME"), Some(&home));
        let unset = r#"["UWE_SURELY_UNSET_VARIABLE"]"#.into();
        spec.environment
            .insert(key, task_model::EnvironmentValue::Literal(unset));
        assert!(with_environment(&spec, &process).is_err());
    }
}
