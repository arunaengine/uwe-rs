//! Tasks built by a language runtime: a Nextflow process, a Snakemake job, an Argo template
//! or a WDL task, run as one task instance with ready inputs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::closure::{Binding, Scope};
use crate::document::Language;
use crate::reserved::RuleKey;

/// Who builds the task. The language keeps its own task construction, acceptance, retry,
/// publication and cache key; it does not run the workflow around the task.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskBuilder {
    NextflowProcess(TaskRef),
    SnakemakeJob(RuleTask),
    ArgoTask(TaskRef),
    WdlTask(TaskRef),
}

impl TaskBuilder {
    /// The reference to the native task.
    pub fn task(&self) -> &TaskRef {
        match self {
            TaskBuilder::NextflowProcess(task)
            | TaskBuilder::ArgoTask(task)
            | TaskBuilder::WdlTask(task) => task,
            TaskBuilder::SnakemakeJob(rule) => &rule.task,
        }
    }

    /// The language whose runtime builds the task.
    pub fn language(&self) -> Language {
        match self {
            TaskBuilder::NextflowProcess(_) => Language::Nextflow,
            TaskBuilder::SnakemakeJob(_) => Language::Snakemake,
            TaskBuilder::ArgoTask(_) => Language::Argo,
            TaskBuilder::WdlTask(_) => Language::Wdl,
        }
    }
}

/// A native task in a pinned source: `entry` runs inside it, `call` is the qualified name of
/// the invocation, and the port maps join wir ports to native selectors.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRef {
    pub source: String,
    pub entry: String,
    pub call: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<Scope>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<PortMap>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<PortMap>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub profiles: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub configs: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub captures: BTreeMap<String, Binding>,
}

/// A wir port and the native selector it stands for, like `input/0` of a Nextflow process.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortMap {
    pub port: String,
    pub native: String,
}

/// A Snakemake job: a rule with its wildcard values, and the settings of the plan it is in.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleTask {
    pub task: TaskRef,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub wildcards: BTreeMap<String, String>,
    /// The other jobs that run in the same task, as a group job, a pipe or a service.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub group: Vec<RuleKey>,
    /// The targets of the plan when there are several, `entry` first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<String>,
    /// The id of a source with the config values of the command line, `--config`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_values: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub until: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub omit_from: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forcerun: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub forceall: bool,
    /// A digest of the files the job reads and makes and of those that other jobs make, as
    /// planned; a run that plans another layout refuses the job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<String>,
}

fn is_false(value: &bool) -> bool {
    !*value
}
