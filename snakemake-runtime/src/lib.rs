//! Runs Snakemake workflows: evaluates the Snakefile natively, plans the job graph backwards
//! from the targets, and asks for one task per job. The Python of Snakefiles runs in
//! `python-evaluator`; `script:`, `wrapper:`, `notebook:` and `run:` jobs become tasks that
//! run their code in a container.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod files;
mod pattern;
mod rule;

pub use pattern::{Matcher, Wildcards, apply as apply_wildcards, regex_text, wildcard_names};
