//! Operational logging on standard error.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use tracing_subscriber::EnvFilter;

use crate::{Failure, arguments::Arguments};

pub fn init(arguments: &Arguments) -> Result<(), Failure> {
    let level = if arguments.quiet {
        "off".to_owned()
    } else if arguments.verbose {
        "debug".to_owned()
    } else {
        std::env::var("IUNCTUS_LOG").unwrap_or_else(|_| "warn".into())
    };
    let filter = EnvFilter::try_new(level)
        .map_err(|error| Failure::usage(format!("iunctus: invalid IUNCTUS_LOG filter: {error}")))?;
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .without_time()
        .with_ansi(false)
        .try_init()
        .map_err(|error| Failure::setup(format!("iunctus: cannot start logging: {error}")))
}
