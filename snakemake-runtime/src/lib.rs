//! Runs Snakemake workflows: evaluates the Snakefile natively, plans the job graph backwards
//! from the targets, and asks for one task per job. The Python of Snakefiles runs in
//! `python-evaluator`; `script:`, `wrapper:`, `notebook:` and `run:` jobs become tasks that
//! run their code in a container, and so do the Python packages without a stand-in that the
//! Snakefile imports (decision 19).
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
mod plan;
mod profile;
mod python;
mod remote;
mod resources;
mod rule;
mod schema;
mod services;
mod shell;
mod sources;
mod storage;
mod task;
mod utils;
mod workflow;

pub use driver::{Session, Settings, driver, driver_with, find_snakefile};
pub use pattern::{Matcher, Wildcards, apply as apply_wildcards, regex_text, wildcard_names};
pub use python::{ContainerStarter, DEFAULT_IMAGE, PythonContainers};
