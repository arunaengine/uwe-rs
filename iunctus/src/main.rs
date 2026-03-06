//! The `iunctus` command: `iunctus run <script> [-stub] [-store <directory>]`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

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
    if command != "run" {
        return usage();
    }
    let mut script = None;
    let mut stub = false;
    let mut store = None;
    let mut rest = rest.iter();
    while let Some(argument) = rest.next() {
        match argument.as_str() {
            "-stub" | "-stub-run" => stub = true,
            "-store" => {
                store = rest.next().map(|directory| {
                    std::path::absolute(directory).unwrap_or_else(|_| directory.into())
                })
            }
            other if script.is_none() && !other.starts_with('-') => {
                script = Some(PathBuf::from(other))
            }
            other => {
                eprintln!("iunctus: unknown option `{other}`");
                return usage();
            }
        }
    }
    let Some(script) = script else {
        return usage();
    };
    let cache = cache_directory();
    let settings = nextflow::Settings {
        store: store.unwrap_or_else(|| cache.join("store")),
        scratch: cache.join("scratch"),
        engine: std::env::var("IUNCTUS_ENGINE").unwrap_or_else(|_| "docker".into()),
        stub,
    };
    match nextflow::run(&script, &settings) {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            eprintln!("{}", failure.message);
            ExitCode::from(if failure.unsupported { UNSUPPORTED } else { 1 })
        }
    }
}

fn usage() -> ExitCode {
    eprintln!("usage: iunctus run <script.nf> [-stub] [-store <directory>]");
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
