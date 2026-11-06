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
            Some("tes" | "slurm" | "kubernetes" | "aws-batch")
        ),
        local_options: !matches!(
            arguments.executor.as_deref(),
            Some("tes" | "slurm" | "kubernetes" | "aws-batch")
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
    ("k8s", "kubernetes"),
    ("slurm", "slurm"),
    ("tes", "tes"),
];

/// For a Nextflow run: the uwe executor that the pipeline's `process.executor` picks, when no
/// `-executor` chose one; and for AWS Batch the settings of the `aws` scope.
pub fn pipeline_executor(
    language: Language,
    launch: &Launch,
    arguments: &Arguments,
    setup: &mut Setup,
) -> Result<Option<String>, crate::Failure> {
    let chosen = arguments.executor.is_some() && !arguments.executor_default;
    let batch = arguments.executor.as_deref() == Some("aws-batch");
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
    if batch || picked.as_deref() == Some("aws-batch") {
        aws_scope(&config, &mut setup.executors.aws_batch);
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
