//! Resolves local Conda environment files into inspected images before task cache lookup.
// Copyright (c) 2026 The iunctus Contributors
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
}

impl Backend for Local {
    async fn pin(&self, image: &str) -> Result<Option<String>, ExecutorError> {
        let Some(source) = image.strip_prefix("conda-env:") else {
            return self.backend.pin(image).await;
        };
        let config = self.config.clone().ok_or_else(|| ExecutorError::Unsupported(
            "Conda rules require explicit [conda] configuration with a pinned micromamba base, target capabilities, repodata and packages".into(),
        ))?;
        let source = source.to_owned();
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
            Ok(Some(receipt.image_id().to_owned()))
        };
        tokio::time::timeout(Duration::from_secs(1200), work)
            .await
            .map_err(|_| failed("preparation exceeded 1200 seconds".into()))?
    }

    async fn run(
        &self,
        job: Job<'_>,
        cancel: &CancellationToken,
    ) -> Result<Finished, ExecutorError> {
        self.backend.run(job, cancel).await
    }

    fn hold(&self) {
        self.backend.hold();
    }
}
