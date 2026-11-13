//! Reads the settings of the AWS Batch executor from `[executor.aws-batch]`: queue, roles, logs,
//! volumes, spot retries, fair share, Fargate, polling and where the jobs get the uwe helper.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::time::Duration;

use config_file::{ConfigError, Table};
use executor::batch::{Helper, TASK_FOLDER, helper};
use storage::aws::{Credentials, Keys};

/// How often AWS Batch is asked about the jobs, by default, as Nextflow does.
const POLL: Duration = Duration::from_secs(10);

/// The settings of the executor. The Nextflow `aws` scope fills those that the config file of
/// uwe leaves unset.
#[derive(Clone, Debug)]
pub struct Settings {
    /// The region of Batch and CloudWatch Logs; else `AWS_REGION` or `AWS_DEFAULT_REGION`.
    pub region: Option<String>,
    /// The job queue of tasks that name none.
    pub queue: Option<String>,
    pub job_role: Option<String>,
    /// The role that pulls the image and writes the log, which Fargate needs.
    pub execution_role: Option<String>,
    /// The CloudWatch Logs group of the jobs; `None` for AWS's `/aws/batch/job`.
    pub logs_group: Option<String>,
    /// Host folders that every job sees.
    pub volumes: Vec<Volume>,
    /// How often AWS Batch runs a job again after its spot instance went away; 0 for never.
    pub spot_attempts: u32,
    /// The fair-share identifier of the jobs, and their priority in it.
    pub share: Option<String>,
    pub priority: i64,
    /// Whether a job that no compute environment of its queue can ever run is stopped, and its
    /// task fails, instead of waiting with a warning.
    pub terminate_unschedulable: bool,
    /// Whether the jobs run on Fargate instead of EC2.
    pub fargate: bool,
    pub poll: Duration,
    /// The profile of the shared AWS files, instead of `AWS_PROFILE`.
    pub profile: Option<String>,
    /// Access keys, as Nextflow's `aws.accessKey` and `aws.secretKey` give them.
    pub keys: Option<Keys>,
    /// The folder of the container that holds the folders of attempts, which must be writable
    /// for the user of the image.
    pub task_folder: String,
    /// Where the jobs get the static uwe binary; `None` for a binary that uwe uploads.
    pub helper: Option<Helper>,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            region: None,
            queue: None,
            job_role: None,
            execution_role: None,
            logs_group: None,
            volumes: Vec::new(),
            spot_attempts: 0,
            share: None,
            priority: 0,
            terminate_unschedulable: false,
            fargate: false,
            poll: POLL,
            profile: None,
            keys: None,
            task_folder: TASK_FOLDER.into(),
            helper: None,
        }
    }
}

/// A host folder that the jobs see, from `<host>`, `<host>:<container>` or
/// `<host>:<container>:ro`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Volume {
    pub host: String,
    pub container: String,
    pub read_only: bool,
}

impl Volume {
    /// Reads a volume as Nextflow's `aws.batch.volumes` writes it.
    pub fn parse(text: &str) -> Result<Volume, String> {
        let parts: Vec<&str> = text.split(':').collect();
        let clean = |part: &str| match part.trim_end_matches('/') {
            "" => "/".to_owned(),
            part => part.to_owned(),
        };
        let invalid = || format!("not a valid volume: {text}; use host[:container[:ro|rw]]");
        let (host, container, read_only) = match parts.as_slice() {
            [host] => (clean(host), clean(host), false),
            [host, container] => (clean(host), clean(container), false),
            [host, container, "ro"] => (clean(host), clean(container), true),
            [host, container, "rw"] => (clean(host), clean(container), false),
            _ => return Err(invalid()),
        };
        if !host.starts_with('/') || !container.starts_with('/') {
            return Err(invalid());
        }
        Ok(Volume {
            host,
            container,
            read_only,
        })
    }
}

impl Settings {
    pub const KEYS: &'static [&'static str] = &[
        "region",
        "queue",
        "job_role",
        "execution_role",
        "logs_group",
        "volumes",
        "max_spot_attempts",
        "share_identifier",
        "scheduling_priority",
        "terminate_unschedulable",
        "platform",
        "poll_seconds",
        "profile",
        "task_folder",
        "helper_path",
        "helper_url",
        "helper_sha256",
    ];

    /// The settings of `[executor.aws-batch]`.
    pub fn read(table: &Table<'_>) -> Result<Settings, ConfigError> {
        let mut settings = Settings {
            region: table.text("region")?,
            queue: table.text("queue")?,
            job_role: table.text("job_role")?,
            execution_role: table.text("execution_role")?,
            logs_group: table.text("logs_group")?,
            share: table.text("share_identifier")?,
            profile: table.text("profile")?,
            terminate_unschedulable: table.flag("terminate_unschedulable")?.unwrap_or(false),
            ..Settings::default()
        };
        for text in table.texts("volumes")?.unwrap_or_default() {
            let volume =
                Volume::parse(&text).map_err(|message| table.invalid("volumes", message))?;
            settings.volumes.push(volume);
        }
        if let Some(count) = table.integer("max_spot_attempts")? {
            settings.spot_attempts = u32::try_from(count)
                .ok()
                .filter(|count| *count <= 10)
                .ok_or_else(|| table.invalid("max_spot_attempts", "must be between 0 and 10"))?;
        }
        if let Some(priority) = table.integer("scheduling_priority")? {
            if !(0..=9999).contains(&priority) {
                return Err(table.invalid("scheduling_priority", "must be between 0 and 9999"));
            }
            settings.priority = priority;
        }
        settings.fargate = match table.text("platform")?.as_deref() {
            None | Some("ec2") => false,
            Some("fargate") => true,
            Some(_) => return Err(table.invalid("platform", "must be ec2 or fargate")),
        };
        if let Some(seconds) = table.integer("poll_seconds")? {
            if !(1..=3600).contains(&seconds) {
                return Err(table.invalid("poll_seconds", "must be between 1 and 3600"));
            }
            settings.poll = Duration::from_secs(seconds as u64);
        }
        if let Some(folder) = table.text("task_folder")? {
            if !folder.starts_with('/') || folder.split('/').any(|part| part == "..") {
                return Err(table.invalid("task_folder", "must be an absolute folder"));
            }
            settings.task_folder = folder;
        }
        settings.helper = helper(table)?;
        Ok(settings)
    }

    /// The region of the clients: the setting, else `AWS_REGION`, else `AWS_DEFAULT_REGION`.
    pub fn region(&self, variable: impl Fn(&str) -> Option<String>) -> Result<String, String> {
        let set = |name: &str| variable(name).filter(|value| !value.is_empty());
        (self.region.clone())
            .or_else(|| set("AWS_REGION"))
            .or_else(|| set("AWS_DEFAULT_REGION"))
            .ok_or_else(|| {
                "AWS Batch needs a region: set `region` in [executor.aws-batch], `aws.region` in \
                 the Nextflow config, or AWS_REGION"
                    .to_owned()
            })
    }

    /// The credentials of the calls: the keys, else AWS's default chain over the environment,
    /// with the profile of the settings.
    pub fn credentials(&self, variable: impl Fn(&str) -> Option<String>) -> Credentials {
        if let Some(keys) = &self.keys {
            return Credentials::Keys(keys.clone());
        }
        let mut environment: HashMap<String, String> = (storage::aws::VARIABLES.iter())
            .filter_map(|name| Some((name.to_string(), variable(name)?)))
            .collect();
        if let Some(profile) = &self.profile {
            environment.insert("AWS_PROFILE".into(), profile.clone());
        }
        Credentials::Chain {
            environment,
            home: variable("HOME").map(Into::into),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use config_file::ConfigFile;

    fn read(text: &str) -> Result<Settings, ConfigError> {
        let config = ConfigFile::parse(text, std::path::Path::new("uwe.toml"))?;
        let section = config.section("executor")?.expect("a section");
        Settings::read(&section.table("aws-batch")?.expect("a table"))
    }

    #[test]
    fn reads_batch_settings() {
        let text = "[executor.aws-batch]\nregion = 'eu-west-1'\nqueue = 'q'\n\
                    volumes = ['/scratch', '/refs/:/refs:ro']\nmax_spot_attempts = 3\n\
                    share_identifier = 'blue'\nscheduling_priority = 10\nplatform = 'fargate'\n\
                    poll_seconds = 5\nhelper_path = '/opt/uwe/uwe'\n";
        let settings = read(text).expect("settings");
        assert_eq!(settings.region.as_deref(), Some("eu-west-1"));
        assert_eq!(settings.queue.as_deref(), Some("q"));
        let refs = Volume {
            host: "/refs".into(),
            container: "/refs".into(),
            read_only: true,
        };
        assert_eq!(settings.volumes[1], refs);
        assert_eq!((settings.spot_attempts, settings.priority), (3, 10));
        assert!(settings.fargate);
        assert_eq!(settings.poll, Duration::from_secs(5));
        assert_eq!(settings.helper, Some(Helper::Path("/opt/uwe/uwe".into())));
        assert_eq!(settings.task_folder, TASK_FOLDER);
        for wrong in [
            "platform = 'gpu'",
            "max_spot_attempts = -1",
            "volumes = ['/a:/b:/c:/d']",
            "volumes = ['relative']",
            "helper_url = 'https://x/uwe'",
            "helper_path = 'uwe'",
            "task_folder = 'tmp'",
        ] {
            let error = read(&format!("[executor.aws-batch]\n{wrong}\n"));
            assert!(error.is_err(), "{wrong}");
        }
    }

    #[test]
    fn finds_the_region() {
        let settings = Settings::default();
        let variables = |name: &str| (name == "AWS_DEFAULT_REGION").then(|| "us-east-2".into());
        assert_eq!(settings.region(variables).as_deref(), Ok("us-east-2"));
        let error = settings.region(|_| None).expect_err("no region");
        assert!(error.contains("AWS_REGION"), "{error}");
        let profiled = Settings {
            profile: Some("lab".into()),
            ..Settings::default()
        };
        let Credentials::Chain { environment, .. } = profiled.credentials(|_| None) else {
            panic!("the default chain");
        };
        assert_eq!(
            environment.get("AWS_PROFILE").map(String::as_str),
            Some("lab")
        );
    }
}
