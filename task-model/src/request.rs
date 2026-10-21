//! A task request: the hashed specification plus placement and reuse scope, which are not hashed.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::TaskSpec;

/// What a driver asks the runner to execute.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRequest {
    /// Unique inside a run and usable as one path segment, like `FASTQC-12` for task 12 of
    /// the step `FASTQC`. Scratch directories and stored outputs are named after it.
    pub identity: String,
    /// For logs, for example `NFCORE_DEMO:DEMO:FASTQC (SAMPLE1)`.
    pub display: String,
    pub scope: ReuseScope,
    /// Starts at 1; a retry is a new attempt of the same task.
    pub attempt: u32,
    pub spec: TaskSpec,
    pub placement: Placement,
}

/// When a stored result may answer a request (design section 5.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReuseScope {
    /// The specification captures everything: pinned image, inputs by digest, no network.
    Shared,
    /// Something outside the specification may matter, so only resumed runs reuse the result.
    Lineage,
    /// Never reused, as with Nextflow `cache false`.
    None,
}

/// Where a task runs and how big it is. Never part of the cache key, so a retry with more
/// memory keeps its key.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Placement {
    pub cpus: u32,
    pub memory_bytes: Option<u64>,
    pub disk_bytes: Option<u64>,
    pub time_seconds: Option<u64>,
    pub accelerators: Vec<Accelerator>,
    pub preemptible: bool,
    /// The site that runs the task, where the executor has several (tasks near their data).
    pub executor: Option<String>,
    pub queue: Option<String>,
    /// Backend parameters passed through as given, such as TES `backend_parameters`.
    pub parameters: BTreeMap<String, String>,
    /// Whether the task runs without network, where the backend can cut it off.
    #[serde(default)]
    pub offline: bool,
    /// How soon the task should start when tasks wait for the machine: the time, in
    /// milliseconds, of the longest chain of work that follows from its step in earlier runs;
    /// 0 when unknown. Tasks with more after them start first, so that the run ends sooner.
    #[serde(default)]
    pub priority: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Accelerator {
    pub count: u32,
    pub kind: Option<String>,
}
