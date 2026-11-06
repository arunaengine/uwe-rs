//! The Nextflow side of the command: the driver with a host that reads remote files through
//! the storage providers and keeps them in the cache, and `uwe config`.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;
use std::rc::Rc;

use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::{Error, FileMetadata, Host};
use nextflow_evaluator::value::{PathValue, Value};
use nextflow_runtime::driver::{Nextflow, Settings, run_folder};
use storage::{Blocking, StorageError};
use workflow_graph::{Driver, DriverError, Language, Launch};

use crate::arguments::Arguments;
use crate::setup::Setup;

/// Prints what the workflow prints, and downloads remote files into a cache.
struct Console {
    downloads: PathBuf,
    /// Prints to standard error instead of standard output, which then holds only the graph.
    quiet: bool,
    /// Whether `System.out` may write to standard output, which an embedding host keeps.
    print: bool,
    storage: Blocking,
    secrets: std::collections::BTreeMap<String, config_file::Secret>,
    /// Where operators like `collectFile` keep their files: the folder of the run, which the
    /// driver removes. `None` keeps them in the system temporary folder.
    operators: Option<PathBuf>,
}

impl Host for Console {
    fn secret(&mut self, name: &str) -> Result<Option<String>, Error> {
        SecretSource(&self.secrets).secret(name)
    }
    fn print(&mut self, text: &str) {
        if self.quiet {
            eprint!("{text}");
        } else {
            print!("{text}");
        }
    }

    /// `System.out` and `System.err`, which config files of legacy syntax print warnings to.
    fn namespace_method(
        &mut self,
        namespace: &str,
        name: &str,
        arguments: Vec<Value>,
    ) -> Result<Option<Value>, Error> {
        let end = if name == "println" { "\n" } else { "" };
        match (namespace, name, arguments.as_slice()) {
            ("System.out", "println" | "print", [message]) if self.print => {
                print!("{message}{end}")
            }
            ("System.out" | "System.err", "println" | "print", [message]) => {
                eprint!("{message}{end}")
            }
            _ => return Ok(None),
        }
        Ok(Some(Value::Null))
    }

    fn operator_directory(&mut self) -> Result<PathValue, Error> {
        let Some(folder) = &self.operators else {
            return nextflow_evaluator::interpreter::thread_scratch();
        };
        let directory = std::fs::create_dir_all(folder)
            .and_then(|()| {
                tempfile::Builder::new()
                    .prefix("operators-")
                    .tempdir_in(folder)
            })
            .map_err(|error| Error::Thrown {
                exception: Rc::new(Exception::new(
                    "IOException",
                    format!("cannot reserve operator scratch: {error}"),
                )),
                span: Default::default(),
            })?;
        Ok(PathValue::parse(&directory.keep().to_string_lossy(), "/"))
    }

    fn download(&mut self, path: &PathValue) -> Result<PathBuf, Error> {
        let uri = path.uri();
        (self.storage.fetch(&uri, &self.downloads)).map_err(|error| storage_error(&uri, error))
    }

    /// Local directories, and directories of storage providers that can list them, like
    /// buckets.
    fn list(&mut self, directory: &PathValue) -> Result<Vec<(String, bool)>, Error> {
        if directory.prefix.is_none() {
            return nextflow_evaluator::interpreter::list_local(directory);
        }
        let uri = directory.uri();
        let entries = self
            .storage
            .list(&uri)
            .map_err(|error| storage_error(&uri, error))?;
        Ok(entries
            .into_iter()
            .map(|entry| (entry.name, entry.directory))
            .collect())
    }

    fn metadata(&mut self, path: &PathValue) -> Result<Option<FileMetadata>, Error> {
        if path.prefix.is_none() {
            return match std::fs::metadata(&path.path) {
                Ok(metadata) => Ok(Some(FileMetadata::of(&metadata))),
                // As in Java, a path below a file does not exist.
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                    ) =>
                {
                    Ok(None)
                }
                Err(error) => Err(io_error(format!("{}: {error}", path.path))),
            };
        }
        let uri = path.uri();
        let found = (self.storage.metadata(&uri, &self.downloads))
            .map_err(|error| storage_error(&uri, error))?;
        Ok(found.map(|found| FileMetadata {
            directory: found.directory,
            size: found.size,
            modified: 0,
            mode: 0o777,
        }))
    }

    fn write(&mut self, path: &PathValue, bytes: &[u8]) -> Result<(), Error> {
        if path.prefix.is_none() {
            if let Some(parent) = std::path::Path::new(&path.path).parent() {
                std::fs::create_dir_all(parent).map_err(|error| io_error(error.to_string()))?;
            }
            return std::fs::write(&path.path, bytes).map_err(|error| io_error(error.to_string()));
        }
        let file = tempfile::NamedTempFile::new().map_err(|error| io_error(error.to_string()))?;
        std::fs::write(file.path(), bytes).map_err(|error| io_error(error.to_string()))?;
        self.storage
            .upload(file.path(), &path.uri())
            .map_err(|error| storage_error(&path.uri(), error))
    }

    fn copy_file(&mut self, source: &PathValue, target: &PathValue) -> Result<(), Error> {
        let local = match source.prefix {
            Some(_) => self.download(source)?,
            None => PathBuf::from(&source.path),
        };
        if target.prefix.is_some() {
            return self
                .storage
                .upload(&local, &target.uri())
                .map_err(|error| storage_error(&target.uri(), error));
        }
        if let Some(parent) = std::path::Path::new(&target.path).parent() {
            std::fs::create_dir_all(parent).map_err(|error| io_error(error.to_string()))?;
        }
        let mut source = std::fs::File::open(local).map_err(|error| io_error(error.to_string()))?;
        let mut output =
            std::fs::File::create(&target.path).map_err(|error| io_error(error.to_string()))?;
        std::io::copy(&mut source, &mut output).map_err(|error| io_error(error.to_string()))?;
        Ok(())
    }

    fn create_directories(&mut self, path: &PathValue) -> Result<(), Error> {
        if path.prefix.is_none() {
            std::fs::create_dir_all(&path.path).map_err(|error| io_error(error.to_string()))?;
        }
        Ok(())
    }
}

/// An error of storage as an error of the workflow: an address that no storage provider
/// serves is not supported, as before there were providers; other errors are Java's
/// `IOException`.
fn storage_error(uri: &str, error: StorageError) -> Error {
    match error {
        StorageError::NoProvider { .. } => Error::Unsupported {
            message: format!("reading remote files like {uri}"),
            span: Default::default(),
        },
        error => io_error(error.to_string()),
    }
}

fn io_error(message: String) -> Error {
    Error::Thrown {
        exception: Rc::new(Exception::new("IOException", message)),
        span: Default::default(),
    }
}

pub(crate) fn conda_file(source: &str) -> Result<tempfile::NamedTempFile, executor::ExecutorError> {
    let failed = |message: String| {
        executor::ExecutorError::Unsupported(format!("Nextflow Conda: {message}"))
    };
    let value: serde_json::Value =
        serde_json::from_str(source).map_err(|error| failed(error.to_string()))?;
    let (source, mut channels) = match value {
        serde_json::Value::String(source) => (
            source,
            vec!["conda-forge".to_owned(), "bioconda".to_owned()],
        ),
        serde_json::Value::Object(mut value) => {
            let source: String =
                serde_json::from_value(value.remove("packages").unwrap_or_default())
                    .map_err(|error| failed(error.to_string()))?;
            let channels: Vec<String> =
                serde_json::from_value(value.remove("channels").unwrap_or_default())
                    .map_err(|error| failed(error.to_string()))?;
            (source, channels)
        }
        _ => return Err(failed("invalid package reference".into())),
    };
    if source.contains('/') || source.contains('\n') || source.trim().is_empty() {
        return Err(failed(
            "named host environments and prefix paths are unsupported".into(),
        ));
    }
    let mut dependencies = Vec::new();
    for package in source.split_whitespace() {
        let spec = conda_environment::MatchSpec::parse(package)
            .map_err(|error| failed(error.to_string()))?;
        if let Some(channel) = spec.channel
            && !channels.contains(&channel)
        {
            channels.push(channel);
        }
        dependencies.push(package.to_owned());
    }
    let text = serde_json::json!({"channels": channels, "dependencies": dependencies}).to_string();
    let file = tempfile::Builder::new()
        .suffix(".yml")
        .tempfile()
        .map_err(executor::ExecutorError::from)?;
    std::fs::write(file.path(), text).map_err(executor::ExecutorError::from)?;
    Ok(file)
}

struct SecretSource<'a>(&'a std::collections::BTreeMap<String, config_file::Secret>);

impl Host for SecretSource<'_> {
    fn print(&mut self, _: &str) {}

    fn secret(&mut self, name: &str) -> Result<Option<String>, Error> {
        if let Some(secret) = self.0.get(name) {
            return secret
                .reveal()
                .map(Some)
                .map_err(|error| io_error(error.to_string()));
        }
        nextflow_evaluator::secrets::get(self, name)
    }
}

pub(crate) fn secret_file(
    names: &str,
    secrets: &std::collections::BTreeMap<String, config_file::Secret>,
) -> Result<tempfile::NamedTempFile, executor::ExecutorError> {
    let failed = executor::ExecutorError::Unsupported;
    let names: Vec<String> =
        serde_json::from_str(names).map_err(|_| failed("Invalid task secret names".into()))?;
    let mut host = SecretSource(secrets);
    let mut text = String::new();
    for name in names {
        if name.is_empty()
            || name.bytes().enumerate().any(|(index, byte)| {
                !(byte == b'_' || byte.is_ascii_alphabetic() || index > 0 && byte.is_ascii_digit())
            })
        {
            return Err(failed("Invalid task secret name".into()));
        }
        let value = host
            .secret(&name)
            .map_err(|error| std::io::Error::other(error.to_string()))?
            .ok_or_else(|| {
                std::io::Error::other(format!("Required secret is missing: '{name}'"))
            })?;
        text.push_str(&format!(
            "export {name}='{}'\n",
            value.replace('\'', "'\\''")
        ));
    }
    let file = tempfile::NamedTempFile::new()?;
    std::fs::write(file.path(), text)?;
    Ok(file)
}

/// The host of a run; remote files are cached in `downloads` of the cache directory.
fn console(quiet: bool, setup: &Setup) -> Console {
    Console {
        downloads: crate::cache_directory().join("downloads"),
        quiet,
        print: true,
        storage: setup.storage.blocking(),
        secrets: setup.secrets.clone(),
        operators: None,
    }
}

/// The Nextflow driver of a launch. `quiet` sends what the workflow prints to standard error.
pub fn driver(
    launch: &Launch,
    run: &str,
    quiet: bool,
    arguments: &Arguments,
    setup: &Setup,
) -> Result<Box<dyn Driver>, DriverError> {
    let console = Console {
        operators: Some(run_folder(&launch.scratch, run)),
        print: !arguments.no_print,
        ..console(quiet, setup)
    };
    let settings = Settings {
        host: Box::new(console),
        container_engine: Some(setup.engine_kind(arguments)),
        run_name: run.to_owned(),
        command_line: std::env::args().collect::<Vec<_>>().join(" "),
        secret_env: !matches!(
            arguments.executor.as_deref(),
            Some("tes" | "slurm" | "kubernetes" | "aws-batch" | "google-batch" | "azure-batch")
        ),
        local_options: !matches!(
            arguments.executor.as_deref(),
            Some("tes" | "slurm" | "kubernetes" | "aws-batch" | "google-batch" | "azure-batch")
        ),
        repository: crate::repository::origin(
            arguments.target.as_deref(),
            arguments.revision.as_deref(),
            &launch.target,
        ),
    };
    Ok(Box::new(Nextflow::new(launch, settings)?))
}

/// The plugins of the config of a launch that a run skips, as warnings for `uwe check`.
pub fn plugin_warnings(launch: &Launch, setup: &Setup) -> Vec<String> {
    let mut console = console(true, setup);
    nextflow_runtime::driver::plugin_warnings(launch, &mut console)
}

/// The place of the store that the `cloudcache` scope of the config names, or the variable
/// `NXF_CLOUDCACHE_PATH`, for nf-cloudcache. A config that does not load gives none; the run
/// reports its error.
pub fn cloud_cache(launch: &Launch, setup: &Setup) -> Result<Option<String>, String> {
    let mut console = console(true, setup);
    let Ok(config) = nextflow_runtime::driver::config(&launch.target, launch, &mut console) else {
        return Ok(None);
    };
    let variable = std::env::var("NXF_CLOUDCACHE_PATH").ok();
    nextflow_plugins::cloud_cache(&config, variable.as_deref())
}

/// `uwe config`: the config of a project in the flat form of `nextflow config -flat`.
pub fn print_config(launch: &Launch, setup: &Setup) -> Result<(), DriverError> {
    let mut console = console(false, setup);
    let config = nextflow_runtime::driver::config(&launch.target, launch, &mut console)?;
    print!("{}", config.flat());
    Ok(())
}

/// Nextflow's executors in `process.executor` and the uwe executors that they pick when no
/// `-executor` is given; uwe refuses the others.
pub(crate) const EXECUTORS: &[(&str, &str)] = &[
    ("local", "local"),
    ("awsbatch", "aws-batch"),
    ("azurebatch", "azure-batch"),
    ("google-batch", "google-batch"),
    ("k8s", "kubernetes"),
    ("slurm", "slurm"),
    ("tes", "tes"),
];

/// For a Nextflow run: the uwe executor that the pipeline's `process.executor` picks, when no
/// `-executor` chose one; and for AWS Batch, Google Batch and Azure Batch the settings of the
/// `aws`, `google` and `azure` scopes.
pub fn pipeline_executor(
    language: Language,
    launch: &Launch,
    arguments: &Arguments,
    setup: &mut Setup,
) -> Result<Option<String>, crate::Failure> {
    let chosen = arguments.executor.is_some() && !arguments.executor_default;
    let named = arguments.executor.as_deref();
    let batch = matches!(named, Some("aws-batch" | "google-batch" | "azure-batch"));
    if language != Language::Nextflow || (chosen && !batch) {
        return Ok(None);
    }
    let mut console = console(true, setup);
    // A config that does not load fails the run in the driver, with its own message.
    let Ok(config) = nextflow_runtime::driver::config(&launch.target, launch, &mut console) else {
        return Ok(None);
    };
    let picked = match chosen {
        true => None,
        false => {
            choice(&config).map_err(|message| crate::Failure::usage(format!("uwe: {message}")))?
        }
    };
    if named == Some("aws-batch") || picked.as_deref() == Some("aws-batch") {
        aws_scope(&config, &mut setup.executors.aws_batch);
    }
    if named == Some("google-batch") || picked.as_deref() == Some("google-batch") {
        google_scope(&config, &mut setup.executors.google_batch)
            .map_err(|message| crate::Failure::usage(format!("uwe: {message}")))?;
    }
    if named == Some("azure-batch") || picked.as_deref() == Some("azure-batch") {
        azure_scope(&config, &mut setup.executors.azure_batch)
            .map_err(|message| crate::Failure::usage(format!("uwe: {message}")))?;
    }
    Ok(picked)
}

/// The uwe executor of `process.executor` and of the executors of the process selectors, which
/// must all be the same, as uwe runs all tasks of a run with one executor.
fn choice(config: &nextflow_config::Config) -> Result<Option<String>, String> {
    let base = config
        .get(&["process", "executor"])
        .and_then(|value| text(&value));
    let mut named = vec![("process.executor".to_owned(), base.clone())];
    if let Some(Value::Map(scope)) = config.get(&["process"]) {
        for (key, body) in &scope.borrow().entries {
            let key = key.0.to_string();
            if let Value::Map(body) = body
                && (key.starts_with("withName:") || key.starts_with("withLabel:"))
            {
                let executor = body
                    .borrow()
                    .entries
                    .iter()
                    .find(|(name, _)| name.0.to_string() == "executor")
                    .and_then(|(_, value)| text(value));
                if executor.is_some() {
                    named.push((key, executor));
                }
            }
        }
    }
    let mut picked: Vec<(String, &str)> = Vec::new();
    for (place, name) in &named {
        // Processes without an executor run on Nextflow's default, `local`.
        let name = name.as_deref().unwrap_or("local");
        let Some((_, uwe)) = EXECUTORS.iter().find(|(nextflow, _)| *nextflow == name) else {
            return Err(format!(
                "uwe has no executor for Nextflow's `{name}` of {place}; choose one with \
                 -executor"
            ));
        };
        picked.push((format!("`{name}` for {place}"), uwe));
    }
    if picked.iter().any(|(_, uwe)| *uwe != picked[0].1) {
        let named: Vec<String> = picked.into_iter().map(|(place, _)| place).collect();
        return Err(format!(
            "the pipeline names several executors ({}), but uwe runs all tasks of a run with \
             one; choose it with -executor",
            named.join(", ")
        ));
    }
    Ok(base.map(|_| picked[0].1.to_owned()))
}

/// Fills the AWS Batch settings that `[executor.aws-batch]` leaves unset from the Nextflow
/// `aws` scope, `executor.$awsbatch` and an `s3://` work directory.
fn aws_scope(config: &nextflow_config::Config, batch: &mut crate::aws_batch::Config) {
    let get = |path: &[&str]| {
        config
            .get(path)
            .filter(|value| !matches!(value, Value::Null))
    };
    let text_of = |path: &[&str]| get(path).as_ref().and_then(text);
    let settings = &mut batch.settings;
    let fill = |slot: &mut Option<String>, path: &[&str]| {
        if slot.is_none() {
            *slot = text_of(path);
        }
    };
    fill(&mut settings.region, &["aws", "region"]);
    fill(&mut settings.profile, &["aws", "profile"]);
    fill(&mut settings.job_role, &["aws", "batch", "jobRole"]);
    fill(
        &mut settings.execution_role,
        &["aws", "batch", "executionRole"],
    );
    fill(&mut settings.logs_group, &["aws", "batch", "logsGroup"]);
    fill(&mut settings.share, &["aws", "batch", "shareIdentifier"]);
    let keys = (text_of(&["aws", "accessKey"])).zip(text_of(&["aws", "secretKey"]));
    if settings.keys.is_none()
        && let Some((id, secret)) = keys
    {
        settings.keys = Some(storage::aws::Keys {
            id,
            secret,
            token: None,
        });
    }
    let number = |path: &[&str]| text_of(path).and_then(|text| text.trim().parse::<i64>().ok());
    if settings.spot_attempts == 0 {
        let attempts = number(&["aws", "batch", "maxSpotAttempts"])
            .and_then(|count| u32::try_from(count).ok());
        settings.spot_attempts = attempts.unwrap_or(0);
    }
    if settings.priority == 0 {
        settings.priority = number(&["aws", "batch", "schedulingPriority"]).unwrap_or(0);
    }
    if !settings.terminate_unschedulable {
        let stop = text_of(&["aws", "batch", "terminateUnschedulableJobs"]);
        settings.terminate_unschedulable = stop.as_deref() == Some("true");
    }
    if !settings.fargate {
        settings.fargate = text_of(&["aws", "batch", "platformType"]).as_deref() == Some("fargate");
    }
    if settings.volumes.is_empty() {
        let volumes = match get(&["aws", "batch", "volumes"]) {
            Some(Value::List(list)) => list.borrow().items.iter().filter_map(text).collect(),
            Some(value) => text(&value)
                .map(|text| text.split(',').map(|part| part.trim().to_owned()).collect())
                .unwrap_or_default(),
            None => Vec::new(),
        };
        for volume in volumes.iter().filter(|volume| !volume.is_empty()) {
            match aws_batch_executor::Volume::parse(volume) {
                Ok(volume) => settings.volumes.push(volume),
                Err(message) => {
                    tracing::warn!(message, "uwe ignores a volume of aws.batch.volumes")
                }
            }
        }
    }
    let poll = match get(&["executor", "$awsbatch", "pollInterval"]) {
        Some(Value::Duration(millis)) => Some(millis),
        Some(value) => {
            text(&value).and_then(|text| nextflow_evaluator::units::parse_duration(&text).ok())
        }
        None => None,
    };
    if let Some(millis) = poll.filter(|millis| *millis >= 1000) {
        settings.poll = std::time::Duration::from_millis(millis as u64);
    }
    if get(&["aws", "batch", "cliPath"]).is_some() {
        tracing::warn!("uwe ignores aws.batch.cliPath; its AWS Batch jobs need no AWS CLI");
    }
    let work = text_of(&["workDir"]).filter(|work| work.starts_with("s3://"));
    if batch.staging.root.is_none()
        && let Some(work) = work
    {
        batch.staging.root = Some(work);
        batch.staging.mode = crate::staging::Mode::ObjectStore;
    }
}

/// Fills the Google Batch settings that `[executor.google-batch]` leaves unset from the
/// Nextflow `google` scope, `executor.$google-batch` and a `gs://` work directory. Fusion is
/// refused, as it needs privileged containers.
fn google_scope(
    config: &nextflow_config::Config,
    batch: &mut crate::google_batch::Config,
) -> Result<(), String> {
    let get = |path: &[&str]| {
        config
            .get(path)
            .filter(|value| !matches!(value, Value::Null))
    };
    let text_of = |path: &[&str]| get(path).as_ref().and_then(text);
    let flag = |path: &[&str]| text_of(path).as_deref() == Some("true");
    let texts = |path: &[&str]| match get(path) {
        Some(Value::List(list)) => list.borrow().items.iter().filter_map(text).collect(),
        Some(value) => text(&value)
            .map(|text| text.split(',').map(|part| part.trim().to_owned()).collect())
            .unwrap_or_default(),
        None => Vec::new(),
    };
    if flag(&["fusion", "enabled"]) {
        return Err(
            "uwe does not run Fusion, as its containers would run privileged; set \
                    fusion.enabled = false"
                .into(),
        );
    }
    let settings = &mut batch.settings;
    let fill = |slot: &mut Option<String>, path: &[&str]| {
        if slot.is_none() {
            *slot = text_of(path);
        }
    };
    let scope = |key: &'static str| ["google", "batch", key];
    fill(&mut settings.project, &["google", "project"]);
    fill(&mut settings.location, &["google", "location"]);
    fill(&mut settings.service_account, &scope("serviceAccountEmail"));
    fill(&mut settings.network, &scope("network"));
    fill(&mut settings.subnetwork, &scope("subnetwork"));
    fill(&mut settings.boot_disk_image, &scope("bootDiskImage"));
    fill(&mut settings.cpu_platform, &scope("cpuPlatform"));
    if settings.logs_path.is_none() {
        let logs = text_of(&scope("logsPath")).filter(|path| path.starts_with("gs://"));
        settings.logs_path = logs;
    }
    settings.spot |= flag(&scope("spot"));
    settings.preemptible |= flag(&scope("preemptible")) && !settings.spot;
    settings.private_address |= flag(&scope("usePrivateAddress"));
    settings.gpu_drivers |= flag(&scope("installGpuDrivers"));
    settings.ops_agent |= flag(&scope("installOpsAgent"));
    if settings.allowed_locations.is_empty() {
        settings.allowed_locations = texts(&scope("allowedLocations"));
    }
    if settings.network_tags.is_empty() {
        settings.network_tags = texts(&scope("networkTags"));
    }
    if settings.spot_attempts == 0 {
        let attempts =
            text_of(&scope("maxSpotAttempts")).and_then(|count| count.trim().parse::<u32>().ok());
        settings.spot_attempts = attempts.unwrap_or(0).min(10);
    }
    let codes: Vec<i64> = (texts(&scope("autoRetryExitCodes")).iter())
        .filter_map(|code| code.trim().parse().ok())
        .collect();
    if !codes.is_empty() {
        settings.retry_codes = codes;
    }
    let disk = match get(&scope("bootDiskSize")) {
        Some(Value::MemoryUnit(bytes)) => u64::try_from(bytes).ok(),
        Some(value) => text(&value)
            .and_then(|text| nextflow_evaluator::units::parse_memory(&text).ok())
            .and_then(|bytes| u64::try_from(bytes).ok()),
        None => None,
    };
    if settings.boot_disk_gb.is_none() {
        settings.boot_disk_gb = disk
            .map(|bytes| bytes.div_ceil(1 << 30))
            .filter(|gb| *gb > 0);
    }
    let poll = match get(&["executor", "$google-batch", "pollInterval"]) {
        Some(Value::Duration(millis)) => Some(millis),
        Some(value) => {
            text(&value).and_then(|text| nextflow_evaluator::units::parse_duration(&text).ok())
        }
        None => None,
    };
    if let Some(millis) = poll.filter(|millis| *millis >= 1000) {
        settings.poll = std::time::Duration::from_millis(millis as u64);
    }
    for (path, why) in [
        (&scope("gcsfuseOptions")[..], "its jobs mount no buckets"),
        (
            &["google", "enableRequesterPaysBuckets"],
            "its storage providers bill the bucket owner",
        ),
        (
            &["executor", "$google-batch", "jobName"],
            "it names its jobs itself",
        ),
    ] {
        if get(path).is_some() {
            tracing::warn!("uwe ignores {}: {why}", path.join("."));
        }
    }
    let work = text_of(&["workDir"]).filter(|work| work.starts_with("gs://"));
    if batch.staging.root.is_none()
        && let Some(work) = work
    {
        batch.staging.root = Some(work);
        batch.staging.mode = crate::staging::Mode::ObjectStore;
    }
    Ok(())
}

/// Fills the Azure Batch settings that `[executor.azure-batch]` leaves unset from the Nextflow
/// `azure` scope, `executor.$azurebatch` and an `az://` work directory. Fusion and privileged
/// pools are refused, as their tasks would run privileged.
fn azure_scope(
    config: &nextflow_config::Config,
    batch: &mut crate::azure_batch::Config,
) -> Result<(), String> {
    use azure_batch_executor::{Hidden, Identity, PRIVILEGED, PoolSettings, Registry};
    let get = |path: &[&str]| {
        config
            .get(path)
            .filter(|value| !matches!(value, Value::Null))
    };
    let text_of = |path: &[&str]| get(path).as_ref().and_then(text);
    let flag = |path: &[&str]| text_of(path).map(|text| text == "true");
    let number = |path: &[&str]| text_of(path).and_then(|text| text.trim().parse::<u32>().ok());
    let millis = |path: &[&str]| match get(path) {
        Some(Value::Duration(millis)) => u64::try_from(millis).ok(),
        Some(value) => text(&value)
            .and_then(|text| nextflow_evaluator::units::parse_duration(&text).ok())
            .and_then(|millis| u64::try_from(millis).ok()),
        None => None,
    };
    if flag(&["fusion", "enabled"]) == Some(true) {
        return Err(
            "uwe does not run Fusion, as its containers would run privileged; set \
             fusion.enabled = false"
                .into(),
        );
    }
    let scope = |key: &'static str| ["azure", "batch", key];
    let settings = &mut batch.settings;
    let fill = |slot: &mut Option<String>, path: &[&str]| {
        if slot.is_none() {
            *slot = text_of(path);
        }
    };
    fill(&mut settings.account, &scope("accountName"));
    fill(&mut settings.endpoint, &scope("endpoint"));
    fill(&mut settings.location, &scope("location"));
    fill(
        &mut settings.tenant_id,
        &["azure", "activeDirectory", "tenantId"],
    );
    let principal = ["azure", "activeDirectory", "servicePrincipalId"];
    fill(&mut settings.client_id, &principal);
    let hidden = |path: &[&str]| text_of(path).map(Hidden::Text);
    if settings.account_key.is_none() {
        settings.account_key = hidden(&scope("accountKey"));
    }
    if settings.client_secret.is_none() {
        settings.client_secret = hidden(&["azure", "activeDirectory", "servicePrincipalSecret"]);
    }
    if settings.managed_identity.is_none() {
        let client = text_of(&["azure", "managedIdentity", "clientId"]).map(Identity::User);
        let system = flag(&["azure", "managedIdentity", "system"]) == Some(true);
        settings.managed_identity = client.or(system.then_some(Identity::System));
    }
    settings.auto_pools |= flag(&scope("autoPoolMode")) == Some(true);
    settings.allow_pool_creation |= flag(&scope("allowPoolCreation")) == Some(true);
    settings.delete_jobs |= flag(&scope("deleteJobsOnCompletion")) == Some(true);
    settings.delete_pools |= flag(&scope("deletePoolsOnCompletion")) == Some(true);
    if settings.delete_tasks.is_none() {
        settings.delete_tasks = flag(&scope("deleteTasksOnCompletion"));
    }
    settings.terminate_jobs &= flag(&scope("terminateJobsOnCompletion")) != Some(false);
    if let Some(millis) = millis(&scope("jobMaxWallClockTime")).filter(|millis| *millis >= 1000) {
        settings.job_time = std::time::Duration::from_millis(millis);
    }
    if let Some(count) = number(&scope("maxJobQuotaRetries")) {
        settings.quota_retries = count;
    }
    if let Some(millis) = millis(&scope("jobQuotaRetryDelay")) {
        settings.quota_wait = std::time::Duration::from_millis(millis);
    }
    let registry = ["azure", "registry"];
    let user = text_of(&[registry[0], registry[1], "userName"]);
    let password = text_of(&[registry[0], registry[1], "password"]);
    if settings.registry.is_none()
        && let (Some(user), Some(password)) = (user, password)
    {
        let server = text_of(&[registry[0], registry[1], "server"]);
        settings.registry = Some(Registry {
            server: server.unwrap_or_else(|| "docker.io".into()),
            user,
            password: Hidden::Text(password),
        });
    }
    let names: Vec<String> = match get(&scope("pools")) {
        Some(Value::Map(pools)) => (pools.borrow().entries.iter())
            .map(|(name, _)| name.0.to_string())
            .collect(),
        _ => Vec::new(),
    };
    for name in &names {
        if settings.pools.contains_key(name) {
            continue;
        }
        let key = |key: &'static str| ["azure", "batch", "pools", name.as_str(), key];
        let start =
            |key: &'static str| ["azure", "batch", "pools", name.as_str(), "startTask", key];
        if flag(&key("privileged")) == Some(true) || flag(&start("privileged")) == Some(true) {
            return Err(format!("azure.batch.pools.{name}.privileged: {PRIVILEGED}"));
        }
        let mut pool = PoolSettings::default();
        let text_or = |slot: &mut String, path: &[&str]| {
            if let Some(value) = text_of(path) {
                *slot = value;
            }
        };
        text_or(&mut pool.vm_size, &key("vmType"));
        text_or(&mut pool.publisher, &key("publisher"));
        text_or(&mut pool.offer, &key("offer"));
        text_or(&mut pool.sku, &key("sku"));
        pool.vm_count = number(&key("vmCount")).unwrap_or(pool.vm_count);
        pool.max_vm_count = number(&key("maxVmCount"));
        pool.auto_scale = flag(&key("autoScale")) == Some(true);
        pool.scale_formula = text_of(&key("scaleFormula"));
        if let Some(millis) = millis(&key("scaleInterval")).filter(|millis| *millis >= 60_000) {
            pool.scale_interval = std::time::Duration::from_millis(millis);
        }
        pool.low_priority = flag(&key("lowPriority")) == Some(true);
        pool.schedule_policy = text_of(&key("schedulePolicy"));
        if !matches!(
            pool.schedule_policy.as_deref(),
            None | Some("spread" | "pack")
        ) {
            let policy = pool.schedule_policy.unwrap_or_default();
            return Err(format!("Unknown Azure Batch scheduling policy: {policy}"));
        }
        pool.run_as = text_of(&key("runAs"));
        pool.image_id = text_of(&key("virtualMachineImageId"));
        pool.unverified_images = flag(&key("allowUnverifiedImages")) == Some(true);
        pool.virtual_network = text_of(&key("virtualNetwork"));
        pool.start_script = text_of(&start("script"));
        settings.pools.insert(name.clone(), pool);
    }
    let poll = millis(&["executor", "$azurebatch", "pollInterval"]);
    if let Some(millis) = poll.filter(|millis| *millis >= 1000) {
        settings.poll = std::time::Duration::from_millis(millis);
    }
    for (path, why) in [
        (&scope("poolIdentityClientId")[..], "it is for Fusion only"),
        (
            &scope("copyToolInstallMode"),
            "its tasks use the uwe helper, not azcopy",
        ),
        (
            &scope("maxParallelTransfers"),
            "its tasks use the uwe helper, not azcopy",
        ),
        (
            &["azure", "azcopy"],
            "its tasks use the uwe helper, not azcopy",
        ),
        (
            &["azure", "storage", "fileShares"],
            "it mounts no Azure Files shares",
        ),
        (&["azure", "retryPolicy"], "it keeps its own retry policy"),
    ] {
        if get(path).is_some() {
            tracing::warn!("uwe ignores {}: {why}", path.join("."));
        }
    }
    let work = text_of(&["workDir"]).filter(|work| work.starts_with("az://"));
    if batch.staging.root.is_none()
        && let Some(work) = work
    {
        batch.staging.root = Some(work);
        batch.staging.mode = crate::staging::Mode::ObjectStore;
    }
    Ok(())
}

/// The text of a config value, without quotes; `None` for one without text.
fn text(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(text) | Value::GString(text) => Some(text.to_string()),
        Value::Path(path) => Some(path.uri()),
        Value::List(_) | Value::Map(_) => None,
        value => Some(value.to_string()),
    }
    .filter(|text| !text.is_empty())
}

#[cfg(test)]
mod tests {
    /// The config of a pipeline whose `nextflow.config` is `text`.
    fn loaded(text: &str) -> nextflow_config::Config {
        let folder = tempfile::tempdir().expect("a folder");
        std::fs::write(folder.path().join("main.nf"), "workflow {}\n").expect("writes");
        std::fs::write(folder.path().join("nextflow.config"), text).expect("writes");
        let launch = workflow_graph::Launch {
            target: folder.path().to_owned(),
            ..workflow_graph::Launch::default()
        };
        let mut host = super::console(true, &crate::setup::Setup::default());
        nextflow_runtime::driver::config(&launch.target, &launch, &mut host).expect("a config")
    }

    #[test]
    fn picks_pipeline_executors() {
        let choice = |text: &str| super::choice(&loaded(text));
        assert_eq!(choice(""), Ok(None));
        let batch = choice("process.executor = 'awsbatch'\n");
        assert_eq!(batch, Ok(Some("aws-batch".to_owned())));
        let same = "process {\n executor = 'k8s'\n withName: FOO { executor = 'k8s' }\n}\n";
        assert_eq!(choice(same), Ok(Some("kubernetes".to_owned())));
        let mixed = "process {\n executor = 'awsbatch'\n withName: FOO { executor = 'local' }\n}\n";
        let error = choice(mixed).expect_err("mixed executors");
        assert!(error.contains("`local` for withName:FOO"), "{error}");
        let error = choice("process.executor = 'sge'\n").expect_err("an unknown executor");
        assert!(error.contains("Nextflow's `sge`"), "{error}");
        // The processes that no selector names run on Nextflow's default, `local`.
        let selected = "process {\n withLabel: big { executor = 'awsbatch' }\n}\n";
        assert!(choice(selected).is_err());
    }

    #[test]
    fn maps_aws_scope() {
        let text = "workDir = 's3://probe/work'\nprocess.executor = 'awsbatch'\n\
                    aws {\n region = 'eu-west-1'\n batch {\n  jobRole = 'arn:role'\n  \
                    maxSpotAttempts = 3\n  volumes = ['/scratch', '/refs:/refs:ro']\n  \
                    logsGroup = '/probe/batch'\n  shareIdentifier = 'blue'\n  \
                    schedulingPriority = 10\n  terminateUnschedulableJobs = true\n }\n}\n\
                    executor {\n $awsbatch {\n  pollInterval = '30 sec'\n }\n}\n";
        let config = loaded(text);
        let mut batch = crate::aws_batch::Config::default();
        batch.settings.region = Some("us-east-1".into());
        super::aws_scope(&config, &mut batch);
        let settings = &batch.settings;
        assert_eq!(
            settings.region.as_deref(),
            Some("us-east-1"),
            "uwe's own setting wins"
        );
        assert_eq!(settings.job_role.as_deref(), Some("arn:role"));
        assert_eq!((settings.spot_attempts, settings.priority), (3, 10));
        assert_eq!(settings.volumes.len(), 2);
        assert!(settings.volumes[1].read_only);
        assert_eq!(settings.logs_group.as_deref(), Some("/probe/batch"));
        assert_eq!(settings.share.as_deref(), Some("blue"));
        assert!(settings.terminate_unschedulable);
        assert_eq!(settings.poll, std::time::Duration::from_secs(30));
        assert_eq!(batch.staging.root.as_deref(), Some("s3://probe/work"));
        assert_eq!(batch.staging.mode, crate::staging::Mode::ObjectStore);
    }

    #[test]
    fn maps_google_scope() {
        let text = "workDir = 'gs://probe/work'\nprocess.executor = 'google-batch'\n\
                    google {\n project = 'lab'\n location = 'europe-west4'\n batch {\n  \
                    spot = true\n  maxSpotAttempts = 3\n  autoRetryExitCodes = [50001, 50002]\n  \
                    bootDiskSize = 60.GB\n  networkTags = ['uwe', 'batch']\n  \
                    usePrivateAddress = true\n  serviceAccountEmail = 'jobs@lab.iam'\n  \
                    logsPath = 'gs://probe/logs'\n }\n}\n\
                    executor.'$google-batch'.pollInterval = '30 sec'\n";
        let config = loaded(text);
        assert_eq!(super::choice(&config), Ok(Some("google-batch".to_owned())));
        let mut batch = crate::google_batch::Config::default();
        batch.settings.location = Some("us-east1".into());
        super::google_scope(&config, &mut batch).expect("a mapping");
        let settings = &batch.settings;
        assert_eq!(settings.project.as_deref(), Some("lab"));
        assert_eq!(settings.location(), "us-east1", "uwe's own setting wins");
        assert!(settings.spot && settings.private_address);
        assert_eq!(settings.spot_attempts, 3);
        assert_eq!(settings.retry_codes, [50001, 50002]);
        assert_eq!(settings.boot_disk_gb, Some(60));
        assert_eq!(settings.network_tags, ["uwe", "batch"]);
        assert_eq!(settings.service_account.as_deref(), Some("jobs@lab.iam"));
        assert_eq!(settings.logs_path.as_deref(), Some("gs://probe/logs"));
        assert_eq!(settings.poll, std::time::Duration::from_secs(30));
        assert_eq!(batch.staging.root.as_deref(), Some("gs://probe/work"));
        let fusion = loaded("fusion.enabled = true\n");
        let mut batch = crate::google_batch::Config::default();
        let error = super::google_scope(&fusion, &mut batch).expect_err("Fusion");
        assert!(error.contains("privileged"), "{error}");
    }

    #[test]
    fn maps_azure_scope() {
        let text = "workDir = 'az://probe/work'\nprocess.executor = 'azurebatch'\n\
                    azure {\n batch {\n  accountName = 'lab'\n  accountKey = 'a2V5'\n  \
                    location = 'westeurope'\n  autoPoolMode = true\n  allowPoolCreation = true\n  \
                    deleteTasksOnCompletion = false\n  terminateJobsOnCompletion = false\n  \
                    jobMaxWallClockTime = '2d'\n  pools {\n   auto {\n    vmType = 'Standard_E2d_v5'\n    \
                    autoScale = true\n    maxVmCount = 12\n    lowPriority = true\n   }\n  }\n }\n \
                    registry {\n  userName = 'me'\n  password = 'pw'\n }\n \
                    activeDirectory.tenantId = 't'\n}\n\
                    executor.'$azurebatch'.pollInterval = '30 sec'\n";
        let config = loaded(text);
        assert_eq!(super::choice(&config), Ok(Some("azure-batch".to_owned())));
        let mut batch = crate::azure_batch::Config::default();
        batch.settings.location = Some("northeurope".into());
        super::azure_scope(&config, &mut batch).expect("a mapping");
        let settings = &batch.settings;
        assert_eq!(settings.account.as_deref(), Some("lab"));
        assert_eq!(
            settings.location.as_deref(),
            Some("northeurope"),
            "uwe's own setting wins"
        );
        let key = settings.account_key.as_ref().map(|key| key.reveal());
        assert_eq!(key, Some(Ok("a2V5".to_owned())));
        assert!(settings.auto_pools && settings.allow_pool_creation);
        assert_eq!(settings.delete_tasks, Some(false));
        assert!(!settings.terminate_jobs);
        assert_eq!(
            settings.job_time,
            std::time::Duration::from_secs(2 * 24 * 3600)
        );
        let pool = &settings.pools["auto"];
        assert_eq!(pool.vm_size, "Standard_E2d_v5");
        assert!(pool.auto_scale && pool.low_priority);
        assert_eq!(pool.max_vm_count, Some(12));
        let registry = settings.registry.as_ref().expect("a registry");
        assert_eq!(
            (registry.server.as_str(), registry.user.as_str()),
            ("docker.io", "me")
        );
        assert_eq!(settings.tenant_id.as_deref(), Some("t"));
        assert_eq!(settings.poll, std::time::Duration::from_secs(30));
        assert_eq!(batch.staging.root.as_deref(), Some("az://probe/work"));
        let privileged = loaded("azure.batch.pools.p.privileged = true\n");
        let mut batch = crate::azure_batch::Config::default();
        let error = super::azure_scope(&privileged, &mut batch).expect_err("privileged");
        assert!(error.contains("least privilege"), "{error}");
    }

    #[test]
    fn writes_local_parents() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("pipeline_info/params.json");
        let mut host = super::console(false, &crate::setup::Setup::default());
        let value = nextflow_evaluator::value::PathValue::parse(&path.to_string_lossy(), "/");
        nextflow_evaluator::interpreter::Host::write(&mut host, &value, b"{}").unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"{}");
    }

    #[test]
    fn reserves_run_operators() {
        let root = tempfile::tempdir().unwrap();
        let run = root.path().join("run");
        let mut host = super::Console {
            operators: Some(run.clone()),
            ..super::console(false, &crate::setup::Setup::default())
        };
        let folder = nextflow_evaluator::interpreter::Host::operator_directory(&mut host).unwrap();
        let folder = std::path::Path::new(&folder.path);
        assert!(folder.starts_with(&run) && folder.is_dir(), "{folder:?}");
    }

    #[test]
    fn protects_task_secrets() {
        use std::os::unix::fs::PermissionsExt;
        let config = config_file::ConfigFile::parse(
            "[secrets]\nTOKEN = \"a b'c$HOME\"\n",
            std::path::Path::new("config.toml"),
        )
        .unwrap();
        let secret = config
            .section("secrets")
            .unwrap()
            .unwrap()
            .secret("TOKEN")
            .unwrap()
            .unwrap();
        let values = std::collections::BTreeMap::from([("TOKEN".into(), secret)]);
        let file = super::secret_file("[\"TOKEN\"]", &values).unwrap();
        assert_eq!(
            file.as_file().metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );
        let output = std::process::Command::new("bash")
            .args(["-c", "source \"$1\"; printf '%s' \"$TOKEN\"", "bash"])
            .arg(file.path())
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"a b'c$HOME");
        let path = file.path().to_owned();
        drop(file);
        assert!(!path.exists());
        let error = super::secret_file("[\"MISSING\"]", &values)
            .unwrap_err()
            .to_string();
        assert!(error.contains("Required secret is missing"));
        assert!(!error.contains("a b'c"));
    }

    #[test]
    fn writes_conda_packages() {
        let file = super::conda_file(
            &serde_json::to_string("bioconda::fastqc=0.12.1 samtools=1.20").unwrap(),
        )
        .unwrap();
        let text = std::fs::read_to_string(file.path()).unwrap();
        let environment = conda_environment::Environment::parse(&text).unwrap();
        assert_eq!(environment.channels, ["conda-forge", "bioconda"]);
        assert_eq!(environment.dependencies.len(), 2);
        assert_eq!(environment.dependencies[0].name, "fastqc");
        let custom = super::conda_file(
            &serde_json::json!({"packages": "foo=1", "channels": ["custom"]}).to_string(),
        )
        .unwrap();
        let environment =
            conda_environment::Environment::parse(&std::fs::read_to_string(custom.path()).unwrap())
                .unwrap();
        assert_eq!(environment.channels, ["custom"]);
        let path = file.path().to_owned();
        drop(file);
        assert!(!path.exists());
        assert!(
            super::conda_file(&serde_json::to_string("/opt/conda/envs/existing").unwrap()).is_err()
        );
    }
}
