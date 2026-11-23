//! The seam through which a host lets the page start, follow and stop runs and compose tools,
//! with the typed requests and answers that the page, WES and the host share.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;

use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::sync::broadcast;

/// The answer of a studio action, or why the host refused it.
pub type Reply<'a, T = Value> = Pin<Box<dyn Future<Output = Result<T, Refusal>> + Send + 'a>>;

/// What a host lets the page do besides showing the recorded runs. The page calls it on its
/// own runtime, so a slow action must not block that thread. Answers whose shape the host owns,
/// like a form or a check report, stay JSON values.
pub trait Studio: Send + Sync {
    /// The executors and languages that runs of this host can use.
    fn capabilities(&self) -> Capabilities;

    /// The workflows that the host found and the presets that it has configured.
    fn workflows(&self) -> Reply<'_, Vec<WorkflowEntry>>;

    /// The language, input form, support report and outputs of one workflow.
    fn describe(&self, request: LaunchRequest) -> Reply<'_, Described>;

    /// Checks the `values` of a request against the form of its workflow, per field.
    fn validate(&self, request: LaunchRequest) -> Reply<'_>;

    /// Starts a run and answers with its id once it is admitted; the run goes on.
    fn launch(&self, request: LaunchRequest) -> Reply<'_, RunStarted>;

    /// Starts a run again with the workflow and values of a recorded one, with the fields
    /// that `changes` gives instead of those of the run.
    fn repeat(&self, run: String, changes: LaunchRequest) -> Reply<'_, RunStarted>;

    /// Asks a run that this host started to stop; false when it started no such run.
    fn cancel(&self, run: &str) -> bool;

    /// A run that this host started in this process, as far as it has come.
    fn launched(&self, run: &str) -> Option<Launched>;

    /// Every run that this host started in this process, the latest first.
    fn launches(&self) -> Vec<Launched>;

    /// The live output and resource use of a run that goes on, as JSON events with a `kind`.
    fn follow(&self, run: &str) -> Option<broadcast::Receiver<Value>>;

    /// The tools of the library that match a filter, with facets and the skipped files.
    fn tools(&self, filter: ToolFilter) -> Reply<'_>;

    /// The whole definition of one tool version.
    fn tool(&self, id: String, version: String) -> Reply<'_>;

    /// Adds the tools of a file or folder to the library.
    fn scan(&self, request: ScanRequest) -> Reply<'_>;

    /// The tools that can follow an output or produce an input, best first.
    fn suggest(&self, request: SuggestRequest) -> Reply<'_>;

    /// The findings of every node and edge of a composed workflow.
    fn check(&self, request: ComposerRequest) -> Reply<'_>;

    /// A composed workflow as `.uwe.yaml` text, as JSON, or lowered into wir.
    fn export(&self, request: ExportRequest) -> Reply<'_, Exported>;

    /// Reads a `.uwe.yaml` text and adds the tools that it names to the library.
    fn import(&self, request: ImportRequest) -> Reply<'_>;

    /// Lowers a composed workflow and starts it like any other run.
    fn run(&self, request: ComposerRequest) -> Reply<'_, RunStarted>;
}

/// What a studio offers; the page shows the launcher and the combiner only with one.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub executors: Vec<String>,
    pub languages: Vec<String>,
}

/// A workflow that the launcher lists: a file that the host found, or a configured preset.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowEntry {
    pub id: String,
    pub name: String,
    /// The file of the workflow, or the source of a preset.
    pub path: Option<String>,
    /// The language as `-lang` names it, or `composed` for a `.uwe.yaml` file.
    pub language: Option<String>,
    /// The preset that starts it, for a configured workflow.
    pub preset: Option<String>,
    pub description: Option<String>,
}

/// What the launcher, the combiner or a WES client asks to describe, check or start; describing
/// a workflow reads only the fields that name it. Empty texts count as not given, as WES
/// clients send every engine parameter.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LaunchRequest {
    /// The path, folder or address of the workflow.
    pub target: Option<String>,
    pub preset: Option<String>,
    pub revision: Option<String>,
    pub profile: Option<Profiles>,
    pub entry: Option<String>,
    pub language: Option<String>,
    /// The typed input values, by input name.
    pub values: Option<Map<String, Value>>,
    pub executor: Option<String>,
    pub resume: Option<ResumeFrom>,
    pub no_network: Option<bool>,
    pub no_cache: Option<bool>,
    pub default_image: Option<String>,
    pub output_dir: Option<String>,
    /// The WES request that this one comes from, kept as it came for the run log.
    pub wes: Option<Value>,
    /// The only runs that a page for some runs lets this request resume; `None` for any run.
    /// The page sets it, never the client.
    #[serde(skip)]
    pub allowed: Option<BTreeSet<String>>,
}

impl LaunchRequest {
    /// This request with each field that `changes` gives instead of its own.
    pub fn changed(self, changes: LaunchRequest) -> LaunchRequest {
        LaunchRequest {
            target: changes.target.or(self.target),
            preset: changes.preset.or(self.preset),
            revision: changes.revision.or(self.revision),
            profile: changes.profile.or(self.profile),
            entry: changes.entry.or(self.entry),
            language: changes.language.or(self.language),
            values: changes.values.or(self.values),
            executor: changes.executor.or(self.executor),
            resume: changes.resume.or(self.resume),
            no_network: changes.no_network.or(self.no_network),
            no_cache: changes.no_cache.or(self.no_cache),
            default_image: changes.default_image.or(self.default_image),
            output_dir: changes.output_dir.or(self.output_dir),
            wes: changes.wes.or(self.wes),
            allowed: changes.allowed.or(self.allowed),
        }
    }
}

/// Profiles as `a,b` or as a list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Profiles {
    Text(String),
    List(Vec<String>),
}

impl Profiles {
    /// The names of the profiles, without empty ones.
    pub fn names(&self) -> Vec<String> {
        let names: Vec<&str> = match self {
            Profiles::Text(text) => text.split(',').collect(),
            Profiles::List(names) => names.iter().map(String::as_str).collect(),
        };
        (names.into_iter().map(str::trim))
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .collect()
    }
}

/// Which run a launch continues: `true` the last run of the workflow, a text that run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ResumeFrom {
    Flag(bool),
    Run(String),
}

impl ResumeFrom {
    /// `None` to start afresh, `Some(None)` for the last run, `Some(Some(id))` for that run.
    /// The texts `true` and `false` are flags, as WES engine parameters are texts.
    pub fn run(&self) -> Option<Option<&str>> {
        match self {
            ResumeFrom::Flag(false) => None,
            ResumeFrom::Flag(true) => Some(None),
            ResumeFrom::Run(text) if text.is_empty() || text == "false" => None,
            ResumeFrom::Run(text) if text == "true" => Some(None),
            ResumeFrom::Run(text) => Some(Some(text)),
        }
    }
}

/// What the launcher shows of a workflow. A part that cannot be found is `None`, with why in
/// `form_error` or `support_error`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Described {
    pub language: String,
    /// The input form of the workflow, as the host's form response gives it.
    pub form: Option<Value>,
    pub form_error: Option<String>,
    /// What the host runs of the workflow: compiled, native and refused parts.
    pub support: Option<Value>,
    pub support_error: Option<String>,
    pub outputs: Value,
    /// The inputs that a run may set, for a preset that fixes the others; `None` for all.
    pub editable: Option<Vec<String>>,
    /// The executor that a run uses when the request names none.
    pub executor: Option<String>,
}

/// A run that a studio admitted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunStarted {
    pub run_id: String,
    pub state: Phase,
}

impl RunStarted {
    pub fn running(run: impl Into<String>) -> RunStarted {
        RunStarted {
            run_id: run.into(),
            state: Phase::Running,
        }
    }
}

/// The filters of the task explorer, as the query of `/api/tools` gives them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ToolFilter {
    /// Words that the id, title, description or keywords hold.
    pub q: Option<String>,
    /// An ecosystem, like `galaxy`; `all` filters nothing.
    pub kind: Option<String>,
    pub operation: Option<String>,
    pub topic: Option<String>,
    pub input_format: Option<String>,
    pub output_format: Option<String>,
    /// `curated`, `imported` or `unchecked`.
    pub curation: Option<String>,
}

/// A file or folder whose tools the library adds.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScanRequest {
    pub path: String,
}

/// Which tools a suggestion looks for: those that read an output, or make an input.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuggestMode {
    Followers,
    Producers,
}

/// The port that a suggestion starts from: a port of a tool of the library by `tool` and
/// `port`, or a `supply` of followers or a `port` object of producers as given.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SuggestRequest {
    pub mode: SuggestMode,
    #[serde(default)]
    pub tool: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    /// The name of a port of `tool`, or an input port object for producers.
    #[serde(default)]
    pub port: Option<Value>,
    /// What an output gives, for followers without a tool.
    #[serde(default)]
    pub supply: Option<Value>,
    /// EDAM operations that rank the tools that do them first.
    #[serde(default)]
    pub operations: Vec<String>,
}

/// A composed workflow in the JSON form of a `.uwe.yaml` file, to check or to run with the
/// values of its inputs and the options of the run.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComposerRequest {
    pub workflow: Value,
    #[serde(default)]
    pub inputs: Map<String, Value>,
    #[serde(default)]
    pub executor: Option<String>,
    #[serde(default)]
    pub no_network: Option<bool>,
    #[serde(default)]
    pub no_cache: Option<bool>,
    #[serde(default)]
    pub default_image: Option<String>,
    #[serde(default)]
    pub output_dir: Option<String>,
}

/// The forms that a composed workflow exports to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    /// A `.uwe.yaml` file.
    #[default]
    Yaml,
    /// The JSON form of a `.uwe.yaml` file.
    Json,
    /// A verified wir document.
    Wir,
}

/// A composed workflow to export.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportRequest {
    pub workflow: Value,
    #[serde(default)]
    pub format: ExportFormat,
}

/// An exported workflow: its text and the name of a file for it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exported {
    pub text: String,
    pub file: String,
}

/// A `.uwe.yaml` text to import, with the folder that the tools it names are below.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportRequest {
    pub text: String,
    #[serde(default)]
    pub base: Option<String>,
}

/// How far a run that a studio started has come.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Admitted, but not yet in the run store.
    Queued,
    Running,
    Complete,
    Failed,
    /// Asked to stop; it is `Canceled` once it ended.
    Canceling,
    Canceled,
}

impl Phase {
    /// Whether the run has ended, which its live streams then do too.
    pub fn ended(self) -> bool {
        matches!(self, Phase::Complete | Phase::Failed | Phase::Canceled)
    }
}

/// A run that a studio started, with the request that started it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Launched {
    pub run: String,
    pub phase: Phase,
    /// Why the run failed, also when it failed before the run store had it.
    pub error: Option<String>,
    /// When it was admitted, in milliseconds since the Unix epoch.
    pub time: i64,
    pub request: LaunchRequest,
}

impl Launched {
    /// The tags that a WES client gave the run.
    pub fn tags(&self) -> BTreeMap<String, Value> {
        let tags = (self.request.wes.as_ref()).and_then(|wes| wes.get("tags")?.as_object());
        tags.map(|tags| tags.clone().into_iter().collect())
            .unwrap_or_default()
    }
}

/// Why a studio refused an action: an HTTP status and a message in one sentence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub status: StatusCode,
    pub message: String,
}

impl Refusal {
    /// The request itself is wrong, like a value that does not fit.
    pub fn invalid(message: impl Into<String>) -> Refusal {
        Refusal {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    pub fn missing(message: impl Into<String>) -> Refusal {
        Refusal {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
        }
    }

    /// The host could not do what was asked, like a workflow that does not load.
    pub fn failed(message: impl Into<String>) -> Refusal {
        Refusal {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            message: message.into(),
        }
    }

    /// The host runs as many runs as it takes; a later request may succeed.
    pub fn busy(message: impl Into<String>) -> Refusal {
        Refusal {
            status: StatusCode::TOO_MANY_REQUESTS,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_launch_forms() {
        let request: LaunchRequest = serde_json::from_value(json!({
            "target": "main.nf", "profile": "test, docker", "resume": "run-7",
        }))
        .expect("a launch request");
        let profiles = request.profile.as_ref().map(Profiles::names);
        assert_eq!(profiles, Some(vec!["test".into(), "docker".into()]));
        assert_eq!(
            request.resume.as_ref().and_then(ResumeFrom::run),
            Some(Some("run-7"))
        );
        let flag = ResumeFrom::Run("false".into());
        assert_eq!(flag.run(), None);
        let unknown = serde_json::from_value::<LaunchRequest>(json!({"threads": 2}));
        assert!(unknown.is_err(), "an unknown field is refused");
        let changed = request.changed(LaunchRequest {
            executor: Some("local".into()),
            ..LaunchRequest::default()
        });
        assert_eq!(
            (changed.target.as_deref(), changed.executor.as_deref()),
            (Some("main.nf"), Some("local"))
        );
    }
}
