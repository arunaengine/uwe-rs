//! A concrete task that a driver asks for, and what came of it.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The workflow language a driver runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    Nextflow,
    Snakemake,
    Cwl,
    Argo,
}

impl std::fmt::Display for Language {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Language::Nextflow => "nextflow",
            Language::Snakemake => "snakemake",
            Language::Cwl => "cwl",
            Language::Argo => "argo",
        })
    }
}

/// Identifies a task inside one run. Drivers choose the numbers; they must be unique per run.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct TaskId(pub u64);

impl std::fmt::Display for TaskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// One execution in a container: what a TES task holds, plus where the task comes from.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    /// For logs, like `NFCORE_DEMO:DEMO:FASTQC (SAMPLE1)` or `bwa_map (sample=A)`.
    pub name: String,
    /// The process, rule, step or template the task is an instance of.
    pub step: String,
    /// The container image; `None` when the workflow names none.
    pub image: Option<String>,
    /// The program and its arguments, like `["bash", "-ue", ".command.sh"]`.
    pub command: Vec<String>,
    /// The shell script of the task as the workflow wrote it, when there is one. The command
    /// runs it, possibly with code of the driver around it, like Nextflow's capture of `env`
    /// outputs. The mock executors read it to find the files the task makes.
    pub script: Option<String>,
    /// The working directory inside the container; `None` for the executor's default.
    pub workdir: Option<String>,
    pub environment: BTreeMap<String, String>,
    pub inputs: Vec<Input>,
    /// Output paths or shell globs, relative to the working directory unless absolute.
    pub outputs: Vec<String>,
    /// Text for standard input, when the task reads one.
    pub stdin: Option<String>,
    pub resources: Resources,
    /// Tasks this one must wait for besides those that made its input files, like Argo steps
    /// or Snakemake rules ordered without files. Each must be a task that the driver asked for
    /// before this one; the engine starts this task only when all of them have finished.
    pub after: Vec<TaskId>,
}

/// A file or directory the task reads.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Input {
    /// Where the task sees it, relative to the working directory unless absolute.
    pub path: String,
    /// Where it comes from: a local path (possibly an output of an earlier task), or a URL
    /// like `https://`, `s3://` or `gs://`.
    pub source: String,
    pub directory: bool,
    /// Literal content, for a file that the workflow writes itself, like a CWL
    /// `InitialWorkDir` entry or an Argo `raw` artifact.
    pub content: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Resources {
    pub cpus: Option<f64>,
    pub memory_bytes: Option<u64>,
    pub disk_bytes: Option<u64>,
    pub time_seconds: Option<u64>,
    pub gpus: Option<u32>,
}

/// What came of a task.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    pub id: TaskId,
    pub exit_code: i32,
    /// The output files, by the path the driver asked for and the local path that holds them.
    pub files: Vec<OutputFile>,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputFile {
    /// The path relative to the working directory, as the task made it.
    pub path: String,
    /// Where the file is now.
    pub location: PathBuf,
    pub directory: bool,
}
