//! The pools and jobs of a run: the pool of a task, checked or created on first use, the job of
//! the run on each pool, and their end when the run ends.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, HashMap};
use std::time::{SystemTime, UNIX_EPOCH};

use executor::ExecutorError;
use serde_json::json;
use sha2::{Digest, Sha256};
use task_model::Placement;

use crate::client::{BatchError, Client};
use crate::outcome::{api, machine};
use crate::pool::{Creation, Pool};
use crate::settings::{AUTO, Settings};

/// The pools and jobs that the tasks of a run use.
#[derive(Debug)]
pub struct Jobs {
    /// A random part of the job ids of this run.
    session: String,
    used: tokio::sync::Mutex<Used>,
}

/// The pools and jobs of the run.
#[derive(Debug, Default)]
struct Used {
    pools: HashMap<String, Pool>,
    /// The job of the run on each pool, by pool id.
    jobs: BTreeMap<String, String>,
    /// The pools that uwe created, which it may delete at the end of the run when no other run
    /// uses them.
    created: Vec<String>,
}

impl Default for Jobs {
    /// The pools and jobs of a new run, with a fresh random part for its job ids.
    fn default() -> Jobs {
        let now = (SystemTime::now().duration_since(UNIX_EPOCH)).unwrap_or_default();
        let seed = format!("{}-{}", now.as_nanos(), std::process::id());
        let digest = Sha256::digest(seed.as_bytes());
        Jobs {
            session: digest[..4]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
            used: tokio::sync::Mutex::default(),
        }
    }
}

impl Jobs {
    /// The pool of a task: its queue, the pool of the settings, or an auto pool; checked or
    /// created on first use.
    pub async fn pool(
        &self,
        client: &Client,
        config: &Settings,
        name: &str,
        placement: &Placement,
    ) -> Result<Pool, ExecutorError> {
        let refuse = |message: String| ExecutorError::Unsupported(message);
        let named = (placement.queue.clone()).or_else(|| config.pool.clone());
        let (id, settings, labels, create) = match named {
            Some(id) => {
                let settings = config.pool_settings(&id);
                (id, settings, BTreeMap::new(), config.allow_pool_creation)
            }
            None if config.auto_pools => {
                let settings = config.pool_settings(AUTO);
                if settings.vm_size.contains(['*', '?', ',']) {
                    return Err(refuse(format!(
                        "uwe has no table of Azure VM sizes to choose from the pattern {}; name \
                         one VM size with vm_size and vm_cpus",
                        settings.vm_size
                    )));
                }
                let labels = placement.labels.clone();
                (
                    crate::pool::auto_id(&settings, &labels),
                    settings,
                    labels,
                    true,
                )
            }
            None => {
                return Err(refuse(format!(
                    "No Azure Batch pool was specified for task '{name}' - set `pool` or \
                     auto_pools = true in [executor.azure-batch], or the queue of the task, \
                     like Nextflow's `queue` directive"
                )));
            }
        };
        crate::pool::valid_id(&id).map_err(refuse)?;
        let mut used = self.used.lock().await;
        if let Some(pool) = used.pools.get(&id) {
            return Ok(pool.clone());
        }
        let pool = match client.pool(&id).await.map_err(api)? {
            Some(found) => crate::pool::check(&found, &id, &settings).map_err(refuse)?,
            None if create => {
                let cores = crate::pool::cores(&settings, &id).map_err(refuse)?;
                let images = match settings.image_id {
                    Some(_) => Vec::new(),
                    None => client.supported_images().await.map_err(api)?,
                };
                let image = crate::pool::image(&images, &settings).map_err(refuse)?;
                let registry = match &config.registry {
                    Some(registry) => Some((registry, registry.password.reveal().map_err(refuse)?)),
                    None => None,
                };
                let now = jiff::Timestamp::now()
                    .strftime("%Y-%m-%dT%H:%M:%S%.6fZ")
                    .to_string();
                let creation = Creation {
                    id: &id,
                    cores,
                    image,
                    registry,
                    labels: &labels,
                    now: &now,
                };
                let body = crate::pool::body(&creation, &settings);
                client.create_pool(&body).await.map_err(api)?;
                tracing::debug!(pool = id, "[AZURE BATCH] created the pool");
                used.created.push(id.clone());
                Pool {
                    id: id.clone(),
                    vm_size: settings.vm_size.clone(),
                    slots: cores,
                    memory_gb: settings.vm_memory_gb,
                    auto_scale: settings.auto_scale,
                    run_as: settings.run_as.clone(),
                }
            }
            None => {
                return Err(refuse(format!(
                    "Can't find Azure Batch pool '{id}' - Make sure it exists or set \
                     allow_pool_creation = true in [executor.azure-batch]"
                )));
            }
        };
        used.pools.insert(id, pool.clone());
        Ok(pool)
    }

    /// The job of the run on a pool, created on first use. While the account has its most
    /// active jobs, uwe waits and tries again a few times.
    pub async fn job(
        &self,
        client: &Client,
        settings: &Settings,
        pool: &str,
    ) -> Result<String, ExecutorError> {
        let mut used = self.used.lock().await;
        if let Some(job) = used.jobs.get(pool) {
            return Ok(job.clone());
        }
        let id = crate::task::job_id(&self.session, pool);
        let body = json!({
            "id": id,
            "poolInfo": {"poolId": pool},
            "constraints": {"maxWallClockTime": format!("PT{}S", settings.job_time.as_secs())},
        });
        let most = settings.quota_retries;
        let mut attempt = 0;
        loop {
            match client.create_job(&body).await {
                Err(error) if error.names("ActiveJobAndScheduleQuotaReached") && attempt < most => {
                    attempt += 1;
                    let wait = settings.quota_wait;
                    tracing::warn!(
                        "Azure Batch active job quota reached - waiting {wait:?} before retry \
                         (attempt {attempt} of {most})"
                    );
                    tokio::time::sleep(wait).await;
                }
                Err(error) if error.names("ActiveJobAndScheduleQuotaReached") => {
                    return Err(machine(format!(
                        "Azure Batch active job quota reached - exceeded maximum number of \
                         retries ({most}). Consider increasing job_quota_retries in \
                         [executor.azure-batch] or reducing the number of concurrent jobs"
                    )));
                }
                created => {
                    created.map_err(api)?;
                    break;
                }
            }
        }
        tracing::debug!(job = id, pool, "[AZURE BATCH] created the job");
        used.jobs.insert(pool.to_owned(), id.clone());
        Ok(id)
    }

    /// Ends the jobs of the run once their tasks ended, deletes them when asked, and deletes
    /// the pools that uwe created when asked and no job of another run uses them.
    pub async fn release(&self, client: &Client, settings: &Settings) {
        let used = std::mem::take(&mut *self.used.lock().await);
        for job in used.jobs.values() {
            if settings.terminate_jobs {
                Jobs::terminate_job(client, job).await;
            }
            if settings.delete_jobs
                && let Err(error) = client.delete(&format!("/jobs/{job}")).await
            {
                tracing::warn!("Unable to delete Azure Batch job {job} - Reason: {error}");
            }
        }
        if !settings.delete_pools {
            return;
        }
        for pool in &used.created {
            // Another run finds an auto pool by the same id, so its jobs may still use the pool.
            match client.pool_jobs(pool).await {
                Ok(jobs)
                    if jobs
                        .iter()
                        .any(|job| !used.jobs.values().any(|own| own == job)) =>
                {
                    tracing::info!(
                        pool,
                        "[AZURE BATCH] kept the pool, as jobs of another run use it"
                    );
                    continue;
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!("Unable to delete Azure Batch pool {pool} - Reason: {error}");
                    continue;
                }
            }
            if let Err(error) = client.delete(&format!("/pools/{pool}")).await {
                tracing::warn!("Unable to delete Azure Batch pool {pool} - Reason: {error}");
            }
        }
    }

    /// Ends a job once its tasks ended, keeping its pool.
    async fn terminate_job(client: &Client, job: &str) {
        let pool = match client.job(job).await {
            Ok(Some(found)) => found["poolInfo"].clone(),
            Ok(None) => return,
            Err(error) => {
                tracing::warn!("Unable to terminate Azure Batch job {job} - Reason: {error}");
                return;
            }
        };
        let body = json!({"onAllTasksComplete": "terminatejob", "poolInfo": pool});
        match client.patch_job(job, &body).await {
            Err(BatchError::Status {
                status,
                code,
                message,
                ..
            }) if status != 409 => tracing::warn!(
                "Unable to terminate Azure Batch job {job} - Status: {code}, Reason: {message}"
            ),
            Err(error @ BatchError::Request { .. }) => {
                tracing::warn!("Unable to terminate Azure Batch job {job} - Reason: {error}")
            }
            _ => {}
        }
    }
}
