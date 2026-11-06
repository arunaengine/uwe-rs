//! Nextflow's `workflow` metadata, run statistics and trace records, as the plugins that
//! report to network services send them.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use md5::{Digest, Md5};
use serde_json::Value as JsonValue;

use crate::observer::{End, Start, Task};
use crate::report_format::{Json, NEXTFLOW_VERSION};

/// The build number and build time of Nextflow 26.04.6, as `workflow.nextflow` shows them.
pub(crate) const NEXTFLOW_BUILD: i64 = 12646;
pub(crate) const NEXTFLOW_TIMESTAMP: &str = "2026-07-09T18:49:00Z";

/// The fields of a Nextflow trace record, in the order of its JSON form.
pub(crate) const TRACE_FIELDS: [&str; 43] = [
    "task_id",
    "status",
    "hash",
    "name",
    "exit",
    "submit",
    "start",
    "process",
    "tag",
    "module",
    "container",
    "attempt",
    "script",
    "scratch",
    "workdir",
    "queue",
    "cpus",
    "memory",
    "disk",
    "time",
    "env",
    "accelerator",
    "accelerator_type",
    "error_action",
    "complete",
    "duration",
    "realtime",
    "%cpu",
    "cpu_model",
    "rchar",
    "wchar",
    "syscr",
    "syscw",
    "read_bytes",
    "write_bytes",
    "%mem",
    "vmem",
    "rss",
    "peak_vmem",
    "peak_rss",
    "vol_ctxt",
    "inv_ctxt",
    "native_id",
];

/// A field of a task's trace record; `module` is an empty list and other unknown fields null.
pub(crate) fn trace_value(task: &Task, field: &str) -> JsonValue {
    match task.record.get(field) {
        Some(value) => value.clone(),
        None if field == "module" => JsonValue::Array(Vec::new()),
        None => JsonValue::Null,
    }
}

/// A number field of a task's trace record.
pub(crate) fn trace_number(task: &Task, field: &str) -> Option<f64> {
    task.record.get(field).and_then(JsonValue::as_f64)
}

pub(crate) fn trace_text<'a>(task: &'a Task, field: &str) -> &'a str {
    (task.record.get(field))
        .and_then(JsonValue::as_str)
        .unwrap_or_default()
}

/// A run as the service plugins know it: its start, its statistics and how it ended.
pub(crate) struct Run {
    pub(crate) start: Start,
    /// The MD5 of the main script's text in hex, as Nextflow's `workflow.scriptId`.
    pub(crate) script_id: Option<String>,
    pub(crate) ending: Option<Ending>,
}

/// How a run ended, as `workflow` shows it.
pub(crate) struct Ending {
    pub(crate) end: i64,
    pub(crate) success: bool,
    pub(crate) exit_status: i64,
    /// The first line of the failing task's standard error, else of the error.
    pub(crate) error_message: Option<String>,
    pub(crate) error_report: Option<String>,
}

impl Ending {
    pub(crate) fn of(end: &End) -> Ending {
        let exit = (end.task.as_ref())
            .and_then(|task| task.record.get("exit")?.as_i64())
            .filter(|exit| *exit != 0);
        let stderr = (end.task.as_ref())
            .and_then(|task| task.stderr.lines().find(|line| !line.trim().is_empty()))
            .map(|line| line.trim().to_owned());
        let first = (end.error.as_deref()).and_then(|error| error.lines().next());
        Ending {
            end: end.end,
            success: end.success,
            exit_status: match end.success {
                true => 0,
                false => exit.unwrap_or(1),
            },
            error_message: stderr.or(first.map(str::to_owned)),
            error_report: end.error.clone(),
        }
    }
}

impl Run {
    pub(crate) fn new(start: &Start) -> Run {
        let script_id = std::fs::read(&start.script).ok().map(|text| {
            Md5::digest(text)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect()
        });
        Run {
            start: start.clone(),
            script_id,
            ending: None,
        }
    }

    pub(crate) fn end(&mut self, end: &End) {
        self.ending = Some(Ending::of(end));
    }

    /// Nextflow's `workflow` map without `stats`, with times as `time` writes them.
    pub(crate) fn workflow(&self, time: impl Fn(i64) -> Json) -> Json {
        let start = &self.start;
        let ending = self.ending.as_ref();
        let config = &start.config;
        let enabled = |scope: &str| config[scope]["enabled"].as_bool().unwrap_or(false);
        let engine = [
            "docker",
            "podman",
            "singularity",
            "apptainer",
            "charliecloud",
        ]
        .into_iter()
        .find(|engine| enabled(engine));
        let script_name = std::path::Path::new(&start.script)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
        let output = match config["outputDir"].as_str() {
            Some(output) => absolute(&start.launch_dir, output),
            None => format!("{}/results", start.launch_dir),
        };
        let words: Vec<&str> = start.command_line.split_whitespace().collect();
        let platform = ["workflowId", "workflowUrl", "user", "workspace"]
            .into_iter()
            .chain(["computeEnv", "pipeline", "labels", "schedRunId"])
            .map(|key| (key, Json::Null));
        let variable = |name: &str| std::env::var(name).unwrap_or_default();
        Json::object([
            ("runName", Json::from(&start.run_name)),
            ("scriptId", Json::from(self.script_id.clone())),
            ("scriptFile", Json::from(&start.script)),
            ("scriptName", Json::from(script_name)),
            ("repository", Json::from(start.repository.clone())),
            ("commitId", Json::from(start.commit_id.clone())),
            ("revision", Json::from(start.revision.clone())),
            ("start", time(start.start)),
            (
                "complete",
                ending.map_or(Json::Null, |ending| time(ending.end)),
            ),
            (
                "duration",
                Json::from(ending.map(|ending| ending.end - start.start)),
            ),
            (
                "container",
                Json::from(config["process"]["container"].as_str()),
            ),
            ("commandLine", Json::from(&start.command_line)),
            ("nextflow", nextflow()),
            (
                "success",
                Json::from(ending.is_some_and(|ending| ending.success)),
            ),
            ("projectDir", Json::from(&start.project_dir)),
            ("projectName", Json::from(&start.project_name)),
            ("launchDir", Json::from(&start.launch_dir)),
            ("outputDir", Json::from(output)),
            ("workDir", Json::from(&start.work_dir)),
            ("homeDir", Json::from(variable("HOME"))),
            ("userName", Json::from(variable("USER"))),
            (
                "exitStatus",
                Json::from(ending.map(|ending| ending.exit_status)),
            ),
            (
                "errorMessage",
                Json::from(ending.and_then(|ending| ending.error_message.clone())),
            ),
            (
                "errorReport",
                Json::from(ending.and_then(|ending| ending.error_report.clone())),
            ),
            ("profile", Json::from(&start.profile)),
            ("sessionId", Json::from(&start.session_id)),
            ("resume", Json::from(words.contains(&"-resume"))),
            ("stubRun", Json::from(words.contains(&"-stub-run"))),
            ("preview", Json::from(words.contains(&"-preview"))),
            ("containerEngine", Json::from(engine)),
            (
                "wave",
                Json::object([("enabled", Json::from(enabled("wave")))]),
            ),
            (
                "fusion",
                Json::object([
                    ("enabled", Json::from(enabled("fusion"))),
                    ("version", Json::Null),
                ]),
            ),
            ("platform", Json::object(platform)),
            ("configFiles", Json::list(start.config_files.iter())),
            ("manifest", manifest(&start.manifest)),
            (
                "failOnIgnore",
                Json::from(config["workflow"]["failOnIgnore"].as_bool() == Some(true)),
            ),
        ])
    }
}

/// `workflow.nextflow`: the version of Nextflow that uwe runs pipelines as.
fn nextflow() -> Json {
    Json::object([
        ("version", Json::from(NEXTFLOW_VERSION)),
        ("build", Json::from(NEXTFLOW_BUILD)),
        ("timestamp", Json::from(NEXTFLOW_TIMESTAMP)),
        ("enable", Json::object::<&str>([])),
    ])
}

/// `workflow.manifest` with Nextflow's keys: most values as text, `mainScript` `main.nf` by
/// default, and a list of Git submodules joined with commas.
fn manifest(manifest: &JsonValue) -> Json {
    let text = |key: &str| match &manifest[key] {
        JsonValue::Null => Json::Null,
        JsonValue::String(text) => Json::from(text),
        JsonValue::Array(items) if key == "gitmodules" => Json::from(
            (items.iter())
                .map(|item| item.as_str().map_or(item.to_string(), str::to_owned))
                .collect::<Vec<_>>()
                .join(","),
        ),
        other => Json::from(other.to_string()),
    };
    let contributors = match &manifest["contributors"] {
        JsonValue::Null => Json::list(Vec::<Json>::new()),
        other => Json::from(other),
    };
    let main = match text("mainScript") {
        Json::Null => Json::from("main.nf"),
        main => main,
    };
    let recurse = manifest["recurseSubmodules"].as_bool() == Some(true);
    Json::object([
        ("author", text("author")),
        ("contributors", contributors),
        ("defaultBranch", text("defaultBranch")),
        ("description", text("description")),
        ("docsUrl", text("docsUrl")),
        ("doi", text("doi")),
        ("gitmodules", text("gitmodules")),
        ("homePage", text("homePage")),
        ("icon", text("icon")),
        ("license", text("license")),
        ("mainScript", main),
        ("name", text("name")),
        ("nextflowVersion", text("nextflowVersion")),
        ("organization", text("organization")),
        ("recurseSubmodules", Json::from(recurse)),
        ("version", text("version")),
    ])
}

/// A path of the config made absolute against the launch directory.
fn absolute(launch: &str, path: &str) -> String {
    match path.starts_with('/') || path.contains("://") {
        true => path.to_owned(),
        false => format!("{launch}/{}", path.trim_start_matches("./")),
    }
}
