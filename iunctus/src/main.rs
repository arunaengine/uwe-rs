//! The `iunctus` command: `iunctus run` runs a Nextflow script, `iunctus config` prints its
//! config.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod arguments;
mod nextflow;

use std::path::PathBuf;
use std::process::ExitCode;

/// The exit code for a feature that Nextflow supports but iunctus does not yet, as CWL
/// runners use it for unsupported requirements.
const UNSUPPORTED: u8 = 33;

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let Some((command, rest)) = arguments.split_first() else {
        return usage();
    };
    let arguments = match arguments::parse(rest) {
        Ok(arguments) => arguments,
        Err(message) => {
            eprintln!("iunctus: {message}");
            return usage();
        }
    };
    let result = match command.as_str() {
        "run" => run(arguments),
        "config" => nextflow::print_config(&arguments),
        _ => return usage(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            eprintln!("{}", failure.message);
            ExitCode::from(if failure.unsupported { UNSUPPORTED } else { 1 })
        }
    }
}

fn run(arguments: arguments::Arguments) -> Result<(), nextflow::Failure> {
    let Some(target) = &arguments.target else {
        return Err(nextflow::Failure {
            message: "iunctus: `run` needs a script or a project directory".into(),
            unsupported: false,
        });
    };
    let cache = cache_directory();
    let settings = nextflow::Settings {
        store: arguments
            .store
            .clone()
            .unwrap_or_else(|| cache.join("store")),
        scratch: cache.join("scratch"),
        engine: std::env::var("IUNCTUS_ENGINE").unwrap_or_else(|_| "docker".into()),
        stub: arguments.stub,
        mock: match arguments.executor.as_str() {
            "local" => false,
            "mock" => true,
            other => {
                return Err(nextflow::Failure {
                    message: format!("iunctus: unknown executor `{other}`; use `local` or `mock`"),
                    unsupported: false,
                });
            }
        },
    };
    nextflow::run(target, &arguments, &settings)
}

fn usage() -> ExitCode {
    eprintln!(
        "usage: iunctus run <script.nf or project> [-stub] [-profile <names>] [-c <config>] \
         [--<param> <value>] [-executor local|mock] [-store <directory>]\n       \
         iunctus config [<project>] [-profile <names>] [-c <config>] [--<param> <value>]"
    );
    ExitCode::from(2)
}

/// `$XDG_CACHE_HOME/iunctus`, else `~/.cache/iunctus`.
fn cache_directory() -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("iunctus")
}
