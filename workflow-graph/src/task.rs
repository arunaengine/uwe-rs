//! A concrete task that a driver asks for, and what came of it.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::time::SystemTime;

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
    /// outputs. The simulator reads it to find the files the task makes.
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
    /// Whether the workflow lets the task use the network: CWL tools run without it unless
    /// they ask for it with `NetworkAccess`; the other languages do not say, and the run
    /// decides. Executors that can cut a container off the network do so.
    #[serde(default, skip_serializing_if = "Network::is_unstated")]
    pub network: Network,
    /// Tasks this one must wait for besides those that made its input files, like Argo steps
    /// or Snakemake rules ordered without files. Each must be a task that the driver asked for
    /// before this one; the engine starts this task only when all of them have finished.
    pub after: Vec<TaskId>,
}

/// Whether a workflow lets a task use the network.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Network {
    /// The workflow does not say; the run decides, and gives the network unless told not to.
    #[default]
    Unstated,
    /// The workflow lets the task use the network, like a CWL tool with `NetworkAccess`.
    Allowed,
    /// The task runs without network, like a CWL tool without `NetworkAccess`.
    Denied,
}

impl Network {
    fn is_unstated(&self) -> bool {
        *self == Network::Unstated
    }
}

impl Task {
    /// Whether the task does nothing: it has neither a command nor an image, like a CWL
    /// `Operation` or a Snakemake rule without an action. Such a task finishes at once with exit
    /// code 0 and makes no files. A task with an image but no command runs the image's default
    /// command, as an Argo container without `command` and `args` does.
    pub fn is_no_op(&self) -> bool {
        self.command.is_empty() && self.image.is_none()
    }
}

/// A file or directory the task reads.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Whether and how the task may change the input.
    #[serde(default, skip_serializing_if = "Writable::is_no")]
    pub writable: Writable,
}

/// Whether and how a task may change an input.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Writable {
    /// The task only reads it; the default.
    #[default]
    No,
    /// The task gets a copy of its own, which it may change and no other task sees, as a CWL
    /// `InitialWorkDir` entry with `writable: true`.
    Copy,
    /// The task changes the input where it is, and later tasks see the changes, as a CWL input
    /// with `InplaceUpdateRequirement`, or the working directory of Snakemake's `shell()` calls
    /// in the Snakefile. Only a local file or directory can be changed in place, and such a
    /// task never comes from the cache.
    InPlace,
}

impl Writable {
    fn is_no(&self) -> bool {
        *self == Writable::No
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Resources {
    pub cpus: Option<f64>,
    pub memory_bytes: Option<u64>,
    pub disk_bytes: Option<u64>,
    pub time_seconds: Option<u64>,
    pub gpus: Option<u32>,
}

/// What came of an attempt of a task.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    pub id: TaskId,
    /// Which attempt of the task this was, counting from 1; the engine sets it.
    pub attempt: u32,
    pub exit_code: i32,
    /// The output files, by the path the driver asked for and the local path that holds them.
    pub files: Vec<OutputFile>,
    pub stdout: String,
    pub stderr: String,
    /// Whether an earlier result answered the task, so that it did not run.
    pub cached: bool,
    /// The image as the task ran in it, pinned to its digest where it has one; `None` when no
    /// container ran, as in a simulated run.
    pub image: Option<String>,
    /// When the attempt started and ended. The engine fills in what the executor leaves out.
    pub started: Option<SystemTime>,
    pub ended: Option<SystemTime>,
    /// What the attempt used, when the executor could measure it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// The digest of each input, like `sha256:<hex>`, by the path the task sees it at, as the
    /// executor hashed it: a file, or the manifest of a directory. Empty when the executor
    /// does not hash inputs, as the simulator does not.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inputs: BTreeMap<String, String>,
    /// The result of the reproducibility check, when the result came from the cache and the
    /// task ran again to see whether its tool gives the same bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reproducibility: Option<ReproducibilityCheck>,
}

/// A task whose result came from the cache, run again to see whether its tool gives the same
/// bytes from the same inputs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReproducibilityCheck {
    /// The run that made the result in the cache.
    pub earlier_run: String,
    pub verdict: Verdict,
}

/// What the task gave when it ran again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum Verdict {
    /// Every output has the same digest as in the cache.
    Same,
    /// These outputs, by path, have other digests than in the cache, or only one of the two
    /// results has them.
    Differs { paths: Vec<String> },
    /// The task failed this time, with this exit code.
    Failed { exit_code: i32 },
}

/// What an attempt of a task used, as the cgroup of its container counted it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// The CPU time, in microseconds.
    pub cpu_usec: Option<u64>,
    /// The peak of memory, in bytes, page cache included.
    pub memory_peak: Option<u64>,
    /// The bytes read from and written to block devices.
    pub read_bytes: Option<u64>,
    pub write_bytes: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputFile {
    /// The path relative to the working directory, as the task made it.
    pub path: String,
    /// Where the file is now: a path on this machine, or a URL like `s3://bucket/key` of a
    /// store elsewhere.
    pub location: String,
    pub directory: bool,
    /// The size of the file, or of all files in a directory, when the executor knows it.
    pub size: Option<u64>,
    /// The digest of the file, or of the manifest of a directory, like `sha256:<hex>`, when
    /// the executor knows it.
    pub digest: Option<String>,
}
