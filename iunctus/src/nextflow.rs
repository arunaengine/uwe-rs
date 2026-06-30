//! The Nextflow side of the command: the driver with a host that downloads remote files into
//! the cache, and `iunctus config`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;
use std::rc::Rc;

use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::{Error, FileMetadata, Host};
use nextflow_evaluator::value::{PathValue, Value};
use nextflow_runtime::driver::{Nextflow, Settings};
use workflow_graph::{Driver, DriverError, Launch};

use crate::remote;

/// Prints what the workflow prints, and downloads remote files into a cache.
struct Console {
    downloads: PathBuf,
    /// Prints to standard error instead of standard output, which then holds only the graph.
    quiet: bool,
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
        let Some(url) = remote::https_url(path) else {
            return Err(Error::Unsupported {
                message: format!("reading remote files like {}", path.uri()),
                span: Default::default(),
            });
        };
        remote::download(&url, &self.downloads).map_err(io_error)
    }

    /// Local directories, and directories of public S3 and Google Cloud Storage buckets.
    fn list(&mut self, directory: &PathValue) -> Result<Vec<(String, bool)>, Error> {
        if directory.prefix.is_some() {
            return remote::list(directory).map_err(io_error);
        }
        nextflow_evaluator::interpreter::list_local(directory)
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
        match remote::https_url(path) {
            Some(url) => remote::metadata(path, &url, &self.downloads).map_err(io_error),
            None => Err(Error::Unsupported {
                message: format!("reading remote files like {}", path.uri()),
                span: Default::default(),
            }),
        }
    }
}

fn io_error(message: String) -> Error {
    Error::Thrown {
        exception: Rc::new(Exception::new("IOException", message)),
        span: Default::default(),
    }
}

/// The host of a run; remote files are cached in `downloads` of the cache directory.
fn console(quiet: bool) -> Console {
    Console {
        downloads: crate::cache_directory().join("downloads"),
        quiet,
    }
}

/// The Nextflow driver of a launch. `quiet` sends what the workflow prints to standard error.
pub fn driver(launch: &Launch, run: &str, quiet: bool) -> Result<Box<dyn Driver>, DriverError> {
    let settings = Settings {
        host: Box::new(console(quiet)),
        container_engine: Some(crate::engine()),
        run_name: run.to_owned(),
        command_line: std::env::args().collect::<Vec<_>>().join(" "),
    };
    Ok(Box::new(Nextflow::new(launch, settings)?))
}

/// `iunctus config`: the config of a project in the flat form of `nextflow config -flat`.
pub fn print_config(launch: &Launch) -> Result<(), DriverError> {
    let config = nextflow_runtime::driver::config(&launch.target, launch, &mut console(false))?;
    print!("{}", config.flat());
    Ok(())
}
