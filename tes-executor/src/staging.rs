//! Uploads TES inputs by content digest and builds portable task requests.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use content_store::{Store, hash_file};
use executor::{ExecutorError, Job};
use storage::{Storage, StoreLocation};
use task_model::{Digest, EnvironmentValue, FileKind, TreeEntry};

use crate::Settings;
use crate::client::{ExecutorSpec, FileType, Input, Resources, Task};

#[derive(Clone, Debug)]
pub struct ObjectStage {
    pub(crate) source: Store,
    pub(crate) objects: Store,
    pub(crate) settings: Settings,
}

enum Walk {
    Path {
        source: PathBuf,
        path: String,
    },
    Folder {
        reader: tokio::fs::ReadDir,
        path: String,
        empty: bool,
    },
}

impl ObjectStage {
    pub fn configured(
        source: Store,
        storage: &Storage,
        settings: &Settings,
    ) -> Result<Self, String> {
        let root = settings
            .staging_root
            .as_deref()
            .ok_or("TES staging_root is missing")?;
        let StoreLocation::Objects(operator) =
            storage.store(root).map_err(|error| error.to_string())?
        else {
            return Err("TES object-store staging needs an S3 storage provider".into());
        };
        let objects = Store::remote(operator, root).map_err(|error| error.to_string())?;
        Ok(Self {
            source,
            objects,
            settings: settings.clone(),
        })
    }

    /// Uses an S3-addressed store, including an injected operator for protocol tests.
    pub fn new(source: Store, objects: Store, settings: Settings) -> Result<Self, String> {
        if !objects.url("cas").starts_with("s3://") {
            return Err("TES staging objects need an s3:// address".into());
        }
        Ok(Self {
            source,
            objects,
            settings,
        })
    }

    pub(crate) async fn prepare(&self, job: Job<'_>, shell: bool) -> Result<Task, ExecutorError> {
        let spec = job.spec;
        spec.validate().map_err(unsupported)?;
        if spec.arguments.is_empty() {
            return Err(ExecutorError::NoCommand(
                spec.image
                    .as_ref()
                    .ok_or(ExecutorError::NoImage)?
                    .reference
                    .clone(),
            ));
        }
        if spec.stdin.is_some() || job.staging.stdin.is_some() {
            return Err(unsupported("TES object-store staging cannot express stdin"));
        }
        if spec.run_as.is_some() {
            return Err(unsupported(
                "TES cannot express task user and group identity",
            ));
        }
        let work = container_path(spec.workdir.as_deref().unwrap_or(super::backend::WORK), "/")?;
        let shell = shell || shell_command(&spec.arguments) || spec.output_manifest.is_some();
        let helper = usize::from(spec.output_manifest.is_some());
        if shell
            && [
                super::backend::CONTROL,
                super::backend::OUTBOX,
                super::capture::TRANSFER,
            ]
            .iter()
            .any(|folder| {
                Path::new(folder).starts_with(&work) || Path::new(&work).starts_with(folder)
            })
        {
            return Err(unsupported("TES workdir overlaps output capture folders"));
        }
        let mut files = Vec::new();
        let mut directories = job.staging.directories.clone();
        let mut programs = Vec::new();
        for staged in &job.staging.inputs {
            if staged.in_place {
                return Err(unsupported(
                    "TES object-store staging cannot update inputs in place",
                ));
            }
            let inside = container_path(&staged.path, &work)?;
            if staged.executable {
                programs.push(inside.clone());
            }
            let tree = match spec
                .inputs
                .iter()
                .find(|input| input.path == staged.path && input.kind == FileKind::Directory)
            {
                Some(input) => Some(
                    self.source
                        .get_tree(&input.digest)
                        .await
                        .map_err(storage_error)?,
                ),
                None => None,
            };
            let entries: Option<BTreeMap<_, _>> = tree.as_ref().map(|tree| {
                tree.entries
                    .iter()
                    .map(|entry| (entry.path(), entry))
                    .collect()
            });
            let mut pending = vec![Walk::Path {
                source: staged.source.clone(),
                path: inside.clone(),
            }];
            while let Some(next) = pending.pop() {
                let (source, path) = match next {
                    Walk::Path { source, path } => (source, path),
                    Walk::Folder {
                        mut reader,
                        path,
                        empty,
                    } => {
                        let Some(entry) = reader.next_entry().await? else {
                            if empty {
                                directories.push(path);
                            }
                            continue;
                        };
                        let name = entry
                            .file_name()
                            .into_string()
                            .map_err(|_| unsupported("TES paths must be UTF-8"))?;
                        let target = container_path(&format!("{path}/{name}"), "/")?;
                        pending.push(Walk::Folder {
                            reader,
                            path,
                            empty: false,
                        });
                        (entry.path(), target)
                    }
                };
                let metadata = tokio::fs::symlink_metadata(&source).await?;
                if metadata.is_symlink() {
                    return Err(unsupported(
                        "TES FILE inputs cannot preserve symbolic links",
                    ));
                }
                if metadata.is_dir() {
                    let reader = tokio::fs::read_dir(&source).await?;
                    pending.push(Walk::Folder {
                        reader,
                        path,
                        empty: true,
                    });
                } else if metadata.is_file() {
                    let expected = match &entries {
                        Some(entries) => {
                            let relative = path.strip_prefix(&format!("{inside}/")).unwrap_or("");
                            match entries.get(relative).copied() {
                                Some(TreeEntry::File {
                                    digest, executable, ..
                                }) => {
                                    if *executable {
                                        programs.push(path.clone());
                                    }
                                    Some(digest.clone())
                                }
                                _ => {
                                    return Err(unsupported(
                                        "staged directory differs from its content manifest",
                                    ));
                                }
                            }
                        }
                        None => spec
                            .inputs
                            .iter()
                            .find(|input| input.path == staged.path)
                            .map(|input| input.digest.clone()),
                    };
                    files.push((source, path, expected));
                    if files.len() + usize::from(shell) + helper > self.settings.max_inputs {
                        return Err(unsupported("TES expanded inputs exceed max_inputs"));
                    }
                } else {
                    return Err(unsupported("TES inputs must be regular files"));
                }
            }
        }
        if !shell && (!directories.is_empty() || !programs.is_empty()) {
            return Err(unsupported(
                "empty directories and executable inputs require a task shell for TES staging",
            ));
        }
        if files.len() + usize::from(shell) + helper > self.settings.max_inputs {
            return Err(unsupported("TES expanded inputs exceed max_inputs"));
        }
        let prefix = executor::capture::attempt_prefix(job)?;
        let mut task = Task {
            name: job.name.into(),
            resources: Some(Resources {
                cpu_cores: Some(job.placement.cpus.max(1)),
                ram_gb: job
                    .placement
                    .memory_bytes
                    .map(|bytes| bytes as f64 / crate::client::GB),
                disk_gb: job
                    .placement
                    .disk_bytes
                    .map(|bytes| bytes as f64 / crate::client::GB),
                ..Resources::default()
            }),
            executors: vec![ExecutorSpec {
                image: spec
                    .image
                    .as_ref()
                    .ok_or(ExecutorError::NoImage)?
                    .reference
                    .clone(),
                command: spec.arguments.clone(),
                workdir: Some(work.clone()),
                env: spec
                    .environment
                    .iter()
                    .map(|(name, value)| {
                        (
                            name.clone(),
                            match value {
                                EnvironmentValue::Literal(value) => value.clone(),
                                EnvironmentValue::WorkDirectory => work.clone(),
                            },
                        )
                    })
                    .collect(),
            }],
            tags: BTreeMap::from([("uwe-attempt".into(), job.name.into())]),
            ..Task::default()
        };
        super::outputs::declare(&mut task, job, &work, &self.objects, &prefix, shell)?;
        if task.outputs.len() > self.settings.max_outputs {
            return Err(unsupported("TES outputs exceed max_outputs"));
        }
        let mut uploads = tokio::task::JoinSet::new();
        let mut inputs = vec![None; files.len()];
        for (index, (source, path, expected)) in files.into_iter().enumerate() {
            if wildcard(&path) {
                return Err(unsupported("TES input paths cannot contain wildcards"));
            }
            if helper != 0 && Path::new(&path).starts_with(super::backend::HELPER) {
                return Err(unsupported("TES input paths overlap the native helper"));
            }
            super::outputs::check_input(&task, &path)?;
            task.inputs.push(Input {
                path: path.clone(),
                ..Input::default()
            });
            let stage = self.clone();
            uploads.spawn(async move {
                let (digest, _) = hash_file(&source).await.map_err(storage_error)?;
                if expected
                    .as_ref()
                    .is_some_and(|expected| expected != &digest)
                {
                    return Err(storage_error(
                        "staged TES input differs from its content digest",
                    ));
                }
                let url = stage.input_url(&source, &digest).await?;
                Ok::<_, ExecutorError>((
                    index,
                    Input {
                        path,
                        url,
                        kind: FileType::File,
                    },
                ))
            });
            if uploads.len() >= 8 {
                let (index, input) = uploads
                    .join_next()
                    .await
                    .unwrap()
                    .map_err(storage_error)??;
                inputs[index] = Some(input);
            }
        }
        while let Some(result) = uploads.join_next().await {
            let (index, input) = result.map_err(storage_error)??;
            inputs[index] = Some(input);
        }
        task.inputs = inputs
            .into_iter()
            .map(|input| input.expect("every upload completed"))
            .collect();
        if shell {
            let script = super::capture::script(job, &work, &directories, &programs)?;
            let script = super::capture::discover(script, job, &work, &self.settings)?;
            let digest = self
                .objects
                .put_content(script.into_bytes().into())
                .await
                .map_err(storage_error)?;
            task.inputs.push(Input {
                path: super::backend::WRAPPER.into(),
                url: destination(&self.objects, &content_store::layout::content(&digest))?,
                kind: FileType::File,
            });
            task.executors[0].command = vec!["sh".into(), super::backend::WRAPPER.into()];
        }
        if let Some(helper) = self.settings.helper_file.as_ref().filter(|_| helper != 0) {
            if !self
                .objects
                .has_content(&helper.digest)
                .await
                .map_err(storage_error)?
            {
                self.objects
                    .put_file_as(&helper.file, &helper.digest)
                    .await
                    .map_err(storage_error)?;
            }
            task.inputs.push(Input {
                path: super::backend::HELPER.into(),
                url: destination(
                    &self.objects,
                    &content_store::layout::content(&helper.digest),
                )?,
                kind: FileType::File,
            });
        }
        Ok(task)
    }

    /// Removes attempt objects when configured, retaining failures for durable retry.
    pub(crate) async fn try_clean(&self, prefix: &str) -> Result<(), ExecutorError> {
        if !self.settings.staging_cleanup {
            return Ok(());
        }
        let mut objects = self
            .objects
            .inventory(&format!("{prefix}/"))
            .await
            .map_err(storage_error)?;
        loop {
            let object = match objects.next().await {
                Ok(Some(object)) => object,
                Ok(None) => break,
                Err(error) => return Err(storage_error(error)),
            };
            self.objects
                .remove(&object.location)
                .await
                .map_err(storage_error)?;
        }
        Ok(())
    }

    async fn input_url(&self, source: &Path, digest: &Digest) -> Result<String, ExecutorError> {
        if let Some(address) = self
            .source
            .content_address(digest)
            .await
            .map_err(storage_error)?
            && bucket(&address) == bucket(&self.objects.url("cas"))
        {
            return Ok(address);
        }
        if !self
            .objects
            .has_content(digest)
            .await
            .map_err(storage_error)?
        {
            self.objects
                .put_file_as(source, digest)
                .await
                .map_err(storage_error)?;
        }
        destination(&self.objects, &content_store::layout::content(digest))
    }
}

pub(crate) fn destination(objects: &Store, location: &str) -> Result<String, ExecutorError> {
    let mut url = reqwest::Url::parse(&objects.url("")).map_err(storage_error)?;
    let root = storage::address::decode(url.path());
    let path = format!("{}/{location}", root.trim_end_matches('/'));
    url.set_path(&path.replace('%', "%25"));
    Ok(url.to_string())
}

pub(crate) fn container_path(path: &str, work: &str) -> Result<String, ExecutorError> {
    let path = if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("{work}/{path}")
    };
    if path.contains(['\0', '\n', '\r', '\\']) || path.split('/').any(|part| part == "..") {
        return Err(unsupported(
            "TES container paths must be canonical absolute paths",
        ));
    }
    let parts: Vec<_> = path
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    Ok(format!("/{}", parts.join("/")))
}

pub(crate) fn shell_command(command: &[String]) -> bool {
    command
        .first()
        .and_then(|program| {
            PathBuf::from(program)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .is_some_and(|program| {
            matches!(
                program.as_str(),
                "sh" | "bash" | "dash" | "ash" | "ksh" | "zsh"
            )
        })
}

pub(crate) fn wildcard(path: &str) -> bool {
    path.contains(['*', '?', '['])
}

fn bucket(address: &str) -> &str {
    address
        .trim_start_matches("s3://")
        .split('/')
        .next()
        .unwrap_or("")
}

pub(crate) fn unsupported(error: impl std::fmt::Display) -> ExecutorError {
    ExecutorError::Unsupported(error.to_string())
}

pub(crate) fn storage_error(error: impl std::fmt::Display) -> ExecutorError {
    std::io::Error::other(format!("TES staging failed: {error}")).into()
}

#[cfg(test)]
#[path = "staging_tests.rs"]
mod tests;
