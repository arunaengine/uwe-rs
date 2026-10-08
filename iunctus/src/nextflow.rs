//! The Nextflow side of the command: the driver with a host that reads remote files through
//! the storage providers and keeps them in the cache, and `iunctus config`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;
use std::rc::Rc;

use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::{Error, FileMetadata, Host};
use nextflow_evaluator::value::{PathValue, Value};
use nextflow_runtime::driver::{Nextflow, Settings};
use storage::{Blocking, Storage, StorageError};
use workflow_graph::{Driver, DriverError, Launch};

use crate::arguments::Arguments;
use crate::setup::Setup;

/// Prints what the workflow prints, and downloads remote files into a cache.
struct Console {
    downloads: PathBuf,
    /// Prints to standard error instead of standard output, which then holds only the graph.
    quiet: bool,
    storage: Blocking,
}

impl Host for Console {
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
            ("System.out", "println" | "print", [message]) => print!("{message}{end}"),
            ("System.err", "println" | "print", [message]) => eprint!("{message}{end}"),
            _ => return Ok(None),
        }
        Ok(Some(Value::Null))
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
                Ok(metadata) => Ok(Some(FileMetadata {
                    directory: metadata.is_dir(),
                    size: metadata.len(),
                })),
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
        }))
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
    let source: String = serde_json::from_str(source).map_err(|error| failed(error.to_string()))?;
    if source.contains('/') || source.contains('\n') || source.trim().is_empty() {
        return Err(failed(
            "named host environments and prefix paths are unsupported".into(),
        ));
    }
    let mut channels = vec!["conda-forge".to_owned()];
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

/// The host of a run; remote files are cached in `downloads` of the cache directory.
fn console(quiet: bool, storage: &Storage) -> Console {
    Console {
        downloads: crate::cache_directory().join("downloads"),
        quiet,
        storage: storage.blocking(),
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
    let settings = Settings {
        host: Box::new(console(quiet, &setup.storage)),
        container_engine: Some(setup.engine_kind(arguments)),
        run_name: run.to_owned(),
        command_line: std::env::args().collect::<Vec<_>>().join(" "),
    };
    Ok(Box::new(Nextflow::new(launch, settings)?))
}

/// `iunctus config`: the config of a project in the flat form of `nextflow config -flat`.
pub fn print_config(launch: &Launch, storage: &Storage) -> Result<(), DriverError> {
    let mut console = console(false, storage);
    let config = nextflow_runtime::driver::config(&launch.target, launch, &mut console)?;
    print!("{}", config.flat());
    Ok(())
}

#[cfg(test)]
mod tests {
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
        let path = file.path().to_owned();
        drop(file);
        assert!(!path.exists());
        assert!(
            super::conda_file(&serde_json::to_string("/opt/conda/envs/existing").unwrap()).is_err()
        );
    }
}
