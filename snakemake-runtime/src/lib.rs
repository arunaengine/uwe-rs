//! Runs Snakemake workflows: evaluates the Snakefile natively, plans the job graph backwards
//! from the targets, and asks for one task per job. The Python of Snakefiles runs in
//! `python-evaluator`; `script:`, `wrapper:`, `notebook:` and `run:` jobs become tasks that
//! run their code in a container.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod commands;
mod config;
mod driver;
mod files;
mod functions;
mod iofile;
mod ioutils;
mod namespace;
mod objects;
mod paramspace;
mod pathvars;
mod pattern;
mod pep;
mod plan;
mod profile;
mod remote;
mod resources;
mod rule;
mod schema;
mod shell;
mod storage;
mod task;
mod utils;
mod workflow;

pub use driver::{Session, driver, find_snakefile};
pub use pattern::{Matcher, Wildcards, apply as apply_wildcards, regex_text, wildcard_names};
