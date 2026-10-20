//! Plans Snakemake jobs with native Python and runs embedded code and external packages in
//! containers.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod benchmark;
mod commands;
mod config;
mod driver;
mod embedded;
mod files;
mod functions;
mod git;
mod groups;
mod hooks;
mod inputs;
mod iofile;
mod ioutils;
mod metadata;
mod namespace;
mod objects;
mod output_schema;
mod paramspace;
mod pathvars;
mod pattern;
mod pep;
mod pipes;
mod plan;
mod profile;
mod python;
mod remote;
mod resources;
mod rule;
mod scheduling;
mod schema;
mod services;
mod shadow;
mod shared;
mod shell;
mod sources;
mod storage;
mod task;
mod utils;
mod workflow;

pub use driver::{
    RunSettings, Session, Settings, driver, driver_with, driver_with_options, find_snakefile,
};
pub use pattern::{Matcher, Wildcards, apply as apply_wildcards, regex_text, wildcard_names};
pub use python::{ContainerStarter, DEFAULT_IMAGE, PythonContainers};
