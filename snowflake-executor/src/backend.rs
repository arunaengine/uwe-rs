//! Runs task attempts as Snowpark Container Services job services: stages through the staging
//! root, starts the job with `EXECUTE JOB SERVICE`, polls its statement, cancels and drops it.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use executor::batch::{Helper, Source};
use executor::binary::Binary;
use executor::capture::{RECORDS, capture};
use executor::registry::{self, ImageConfigs};
use executor::remote::RemoteStage;
use executor::wrapper::{Folders, wrapper};
use executor::{Access, Backend, CONTROL, ContainerRun, ExecutorError, Finished, HOME, Job};
use task_model::TaskSpec;
use tokio_util::sync::CancellationToken;

use crate::client::{Answer, Client, SqlError};
use crate::job::{MAIN, Shape, job_name, mapped, request_id};
use crate::outcome::{Ended, finish, machine};
use crate::settings::Settings;

/// How many lines of the end of the log of a failed job go into its error.
const LOG_LINES: u32 = 30;

/// How often a short statement, like the one that reads a log, is asked about.
const SHORT_POLL: Duration = Duration::from_secs(1);

/// The Snowflake executor's backend.
#[derive(Debug)]
pub struct SnowflakeBackend {
    pub client: Client,
    pub settings: Settings,
    /// Where each attempt gets the local folder that its results come back to.
    pub scratch: PathBuf,
    /// The staging root, which this machine and the jobs reach.
    pub stage: RemoteStage,
    /// The binary that uwe uploads for the jobs, when the settings name no helper.
    pub upload: Option<Binary>,
    /// How much longer than its task a job may run: for its image pull and staging.
    pub staging_time: Duration,
    /// The entrypoints and default commands of images, for tasks without a command.
    pub configs: ImageConfigs,
    held: AtomicBool,
}

impl SnowflakeBackend {
    /// A backend whose jobs get the helper as `settings` say, or else `upload` from the
    /// staging root.
    pub fn new(
        client: Client,
        settings: Settings,
        scratch: PathBuf,
        stage: RemoteStage,
        upload: Option<Binary>,
    ) -> Result<SnowflakeBackend, String> {
        let named = settings.helper.is_some() || settings.helper_image.is_some();
        if !named && upload.is_none() {
            return Err(
                "Snowflake jobs need the uwe helper: set helper_image, helper_path, \
                        helper_url or helper_file in [executor.snowflake]"
                    .into(),
            );
        }
        if settings.compute_pool.is_none() {
            return Err(
                "the Snowflake executor needs a compute pool: set compute_pool in \
                        [executor.snowflake], or snowflake.computePool"
                    .into(),
            );
        }
        if stage.credentials {
            return Err(
                "Snowflake staging uses presigned requests; leave out staging_access".into(),
            );
        }
        Ok(SnowflakeBackend {
            client,
            settings,
            scratch,
            stage,
            upload,
            staging_time: Duration::from_secs(30 * 60),
            configs: ImageConfigs::default(),
            held: AtomicBool::new(false),
        })
    }
}

impl Backend for SnowflakeBackend {
    async fn pin(&self, image: &str) -> Result<Option<String>, ExecutorError> {
        // Images of Snowflake image repositories need Snowflake's sign-in; they run as named.
        if image.starts_with('/') || image.contains(".registry.snowflakecomputing.com") {
            return Ok(None);
        }
        registry::pin(image).await
    }

    fn hold(&self) {
        self.held.store(true, Ordering::Relaxed);
    }

    async fn run(
        &self,
        job: Job<'_>,
        cancel: &CancellationToken,
    ) -> Result<Finished, ExecutorError> {
        if cancel.is_cancelled() || self.held.load(Ordering::Relaxed) {
            return Err(ExecutorError::Cancelled);
        }
        let mut spec = job.spec.clone();
        admit(job, &spec)?;
        let image = spec.image.as_ref().ok_or(ExecutorError::NoImage)?;
        if spec.arguments.is_empty() {
            // The job runs the entrypoint of the image around the arguments of its container.
            let config = self.configs.get(&image.reference).await?;
            spec.arguments = config.cmd.clone();
        }
        if spec.arguments.is_empty() {
            return Err(ExecutorError::NoCommand(image.reference.clone()));
        }
        let local = self.scratch.join(job.name);
        tokio::fs::create_dir_all(&self.scratch).await?;
        // Another attempt with this name owns its folder.
        tokio::fs::create_dir(&local).await?;
        let mut staged = None;
        let result = self.attempt(job, &spec, &local, &mut staged, cancel).await;
        if let Some((prefix, plan)) = &staged {
            self.stage.clean(prefix, plan).await;
        }
        if result.is_err() {
            let _ = tokio::fs::remove_dir_all(&local).await;
        }
        result
    }
}

/// Refuses what a job service cannot do, before anything is uploaded.
fn admit(job: Job<'_>, spec: &TaskSpec) -> Result<(), ExecutorError> {
    let refuse = |message: &str| Err(ExecutorError::Unsupported(message.to_owned()));
    if job.placement.offline {
        return refuse("Snowflake jobs need the network for object-store staging");
    }
    if !spec.containers.is_empty() {
        return refuse("the Snowflake executor runs only tasks of one container");
    }
    if spec.run_as.is_some() {
        return refuse("Snowflake runs a task as the user of its image; it cannot set one");
    }
    let absolute = (job.staging.inputs.iter().map(|input| &input.path))
        .chain(&job.staging.directories)
        .any(|path| path.starts_with('/'));
    if absolute {
        return refuse("Snowflake object-store staging cannot place inputs at absolute paths");
    }
    Ok(())
}

impl SnowflakeBackend {
    /// Stages an attempt, starts its job service and waits for its end. The objects of the
    /// attempt go to `staged` as soon as they are in the staging root.
    async fn attempt(
        &self,
        job: Job<'_>,
        spec: &TaskSpec,
        local: &Path,
        staged: &mut Option<(String, String)>,
        cancel: &CancellationToken,
    ) -> Result<Finished, ExecutorError> {
        let image = spec.image.as_ref().ok_or(ExecutorError::NoImage)?;
        let root = Path::new(&self.settings.task_folder).join(job.name);
        let access = Access::default();
        let run = ContainerRun {
            name: job.name,
            image,
            root: &root,
            workdir: spec.workdir.as_deref(),
            mounts: &[],
            owner: (0, 0),
            cpus: job.placement.cpus.max(1),
            memory: job.placement.memory_bytes,
            offline: false,
            wrapper_path: None,
            access: &access,
        };
        let (control, home) = (text(&root.join(CONTROL)), text(&root.join(HOME)));
        let folders = Folders {
            work: &run.inside(),
            control: &control,
            home: &home,
            outbox: &text(&root.join("outbox")),
            owner: "",
        };
        let transfer = text(&root.join(executor::helper::TRANSFER));
        let script = capture(&wrapper(spec, &folders), &transfer)?;
        let discovery = executor::helper::Discovery::of(spec, job.staging);
        let extra = [("control/wrapper.sh", script)];
        let planned = (self.stage)
            .plan_discovery(job, &root, &extra, &[], RECORDS, false, discovery)
            .await?;
        *staged = Some((planned.prefix.clone(), planned.plan.clone()));
        let folder = format!("{}.helper", text(&root));
        let placed = format!("{folder}/uwe");
        let named = self.settings.helper.is_some() || self.settings.helper_image.is_some();
        let download = match &self.upload {
            Some(binary) if !named => Some(self.stage.binary(binary).await?),
            _ => None,
        };
        let digest = self.upload.as_ref().map(|binary| binary.digest.hex());
        let source = match (
            &self.settings.helper_image,
            &self.settings.helper,
            &download,
        ) {
            (Some(_), _, _) => Source::Path(&placed),
            (None, Some(Helper::Path(path)), _) => Source::Path(path),
            (None, Some(Helper::Download { url, sha256 }), _) => Source::Download {
                url,
                sha256,
                machine: None,
            },
            (None, None, Some(url)) => Source::Download {
                url,
                sha256: digest.unwrap_or_default(),
                machine: (self.upload.as_ref()).map(|binary| binary.machine.uname()),
            },
            (None, None, None) => {
                return Err(ExecutorError::Unsupported(
                    "no uwe helper for the jobs".into(),
                ));
            }
        };
        let time = job.placement.time_seconds;
        let command = executor::batch::command(&text(&root), &planned.location, source, time);
        let moment = (SystemTime::now().duration_since(SystemTime::UNIX_EPOCH))
            .map_or(0, |elapsed| elapsed.as_nanos());
        let name = job_name(job.name, moment);
        let image_name = mapped(&image.reference, &self.settings.mappings);
        let helper = (self.settings.helper_image.as_deref()).map(|image| (image, folder.as_str()));
        let shape = Shape {
            name: &name,
            attempt: job.name,
            image: &image_name,
            command,
            placement: job.placement,
            helper,
        };
        let statement =
            crate::job::statement(&shape, &self.settings).map_err(ExecutorError::Unsupported)?;
        if cancel.is_cancelled() || self.held.load(Ordering::Relaxed) {
            return Err(ExecutorError::Cancelled);
        }
        let seconds = time.map(|time| time + self.staging_time.as_secs());
        let answer = (self
            .client
            .submit(&statement, &request_id(&name), seconds)
            .await)
            .map_err(api)?;
        tracing::debug!(
            job = name,
            attempt = job.name,
            "[SNOWFLAKE] started the job service"
        );
        let ended = match answer {
            Answer::Running { handle } => self.wait(&handle, &name, cancel).await,
            Answer::Done { .. } => Ok(Ended {
                failed: false,
                message: String::new(),
            }),
            // The job runs after the answer, so an error now is Snowflake refusing it.
            Answer::Failed { message, .. } => {
                return Err(ExecutorError::Unsupported(format!(
                    "Snowflake refused the job service {name}: {message}"
                )));
            }
        };
        let ended = match ended {
            Ok(ended) => ended,
            Err(error) => {
                self.drop_service(&name).await;
                return Err(error);
            }
        };
        let collected =
            executor::remote::patiently(|| self.stage.collect(&planned.prefix, local, RECORDS))
                .await;
        if let Err(error) = collected {
            self.drop_service(&name).await;
            // The objects stay, as they are the only copy of the results.
            *staged = None;
            return Err(error);
        }
        let finished = match finish(&ended, local.to_owned(), time, spec).await {
            Err(ExecutorError::Engine { engine, message }) => {
                let log = self.log(&name).await;
                Err(ExecutorError::Engine {
                    engine,
                    message: format!("{message}{log}"),
                })
            }
            finished => finished,
        };
        self.drop_service(&name).await;
        finished
    }

    /// Polls the statement of the job until it ended, and cancels it when `cancel` fires.
    async fn wait(
        &self,
        handle: &str,
        name: &str,
        cancel: &CancellationToken,
    ) -> Result<Ended, ExecutorError> {
        loop {
            tokio::select! {
                () = tokio::time::sleep(self.settings.poll) => {}
                () = cancel.cancelled() => {
                    self.cancel(handle, name).await;
                    return Err(ExecutorError::Cancelled);
                }
            }
            match self.client.status(handle).await {
                Ok(Answer::Running { .. }) => {}
                Ok(Answer::Done { .. }) => {
                    return Ok(Ended {
                        failed: false,
                        message: String::new(),
                    });
                }
                Ok(Answer::Failed { message, .. }) => {
                    return Ok(Ended {
                        failed: true,
                        message,
                    });
                }
                Err(error) => {
                    self.cancel(handle, name).await;
                    return Err(api(error));
                }
            }
        }
    }

    /// Cancels the statement of a job, which stops the job; a failure leaves it running, so it
    /// is a warning.
    async fn cancel(&self, handle: &str, name: &str) {
        if let Err(error) = self.client.cancel(handle).await {
            tracing::warn!("[SNOWFLAKE] Unable to cancel the job service {name} -- {error}");
        }
    }

    /// Drops the job service, which stays after its end; a failure leaves it, so it is a
    /// warning.
    async fn drop_service(&self, name: &str) {
        let statement = format!("DROP SERVICE IF EXISTS {name}");
        let request = request_id(&format!("{name}\ndrop"));
        match self.client.execute(&statement, &request, SHORT_POLL).await {
            Ok(Answer::Failed { message, .. }) => {
                tracing::warn!("[SNOWFLAKE] Unable to drop the job service {name} -- {message}");
            }
            Err(error) => {
                tracing::warn!("[SNOWFLAKE] Unable to drop the job service {name} -- {error}");
            }
            Ok(_) => {}
        }
    }

    /// The end of the log of the task container, as a note for its error; empty when there is
    /// none.
    async fn log(&self, name: &str) -> String {
        let statement =
            format!("SELECT SYSTEM$GET_SERVICE_LOGS('{name}', '0', '{MAIN}', {LOG_LINES})");
        let request = request_id(&format!("{name}\nlog"));
        match self.client.execute(&statement, &request, SHORT_POLL).await {
            Ok(Answer::Done { data, .. }) => match data[0][0].as_str() {
                Some(text) if !text.trim().is_empty() => format!("\n{}", text.trim_end()),
                _ => String::new(),
            },
            Ok(answer) => {
                tracing::debug!(job = name, ?answer, "cannot read the log of the job");
                String::new()
            }
            Err(error) => {
                tracing::debug!(job = name, %error, "cannot read the log of the job");
                String::new()
            }
        }
    }
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// A failed call: one that may work later is a failure of the machine, others are permanent.
fn api(error: SqlError) -> ExecutorError {
    match error.retryable() {
        true => machine(error.to_string()),
        false => ExecutorError::Unsupported(format!("Snowflake refused a request: {error}")),
    }
}
