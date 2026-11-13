//! The data of nf-report's three reports: the execution events of the run, the finished tasks by
//! status, and the tasks by sample with a status for each sample.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use md5::{Digest, Md5};
use nextflow_evaluator::regex::Pattern;
use nextflow_evaluator::value::Value;

use super::files::{memory, millis_time, now};
use super::settings::Report;
use crate::observer::{End, Start, Task};
use crate::report_format::{Json, NEXTFLOW_VERSION};

/// The statuses of tasks, in the order of `tasks_by_status`.
pub(crate) const STATUSES: [&str; 6] = [
    "COMPLETED",
    "CACHED",
    "FAILED",
    "ABORTED",
    "RETRIED",
    "IGNORED",
];

/// A task attempt that ended or that the cache answered.
#[derive(Clone, Debug)]
pub(crate) struct Finished {
    pub task: Task,
    pub cached: bool,
    pub samples: Vec<String>,
}

impl Finished {
    pub(crate) fn number(&self, key: &str) -> Option<i64> {
        self.task
            .record
            .get(key)
            .and_then(serde_json::Value::as_i64)
    }

    pub(crate) fn text(&self, key: &str) -> Option<String> {
        match self.task.record.get(key)? {
            serde_json::Value::Null => None,
            serde_json::Value::String(text) => Some(text.clone()),
            other => Some(other.to_string()),
        }
    }

    pub(crate) fn attempt(&self) -> i64 {
        self.number("attempt").unwrap_or(1)
    }

    /// The trace status, `CACHED` for a cached task, `COMPLETED` when the trace has none.
    pub(crate) fn trace_status(&self) -> String {
        match (self.cached, self.text("status")) {
            (true, _) => "CACHED".to_owned(),
            (false, Some(status)) => status,
            (false, None) => "COMPLETED".to_owned(),
        }
    }
}

/// The plugin's status of a task: a failed attempt that a later attempt of the same task
/// followed was retried; one that did not end the run was ignored.
pub(crate) fn status(task: &Finished, latest: i64, end: Option<&End>) -> &'static str {
    let trace = task.trace_status();
    match trace.as_str() {
        "CACHED" => "CACHED",
        "FAILED" | "ABORTED" if latest > task.attempt() => "RETRIED",
        "ABORTED" => "ABORTED",
        "FAILED" => match end {
            Some(end) if end.success => "IGNORED",
            Some(end)
                if end
                    .task
                    .as_ref()
                    .is_some_and(|failed| id(&failed.record) != task.text("task_id")) =>
            {
                "IGNORED"
            }
            _ => "FAILED",
        },
        _ => "COMPLETED",
    }
}

/// The id of a task, which its attempts share.
pub(crate) fn id(record: &crate::observer::Record) -> Option<String> {
    record
        .get("task_id")
        .filter(|id| !id.is_null())
        .map(json_text)
}

/// The sample names of a task: its tag, or the meta map of its inputs.
pub(crate) fn samples(task: &Task, report: &Report) -> Vec<String> {
    let pattern =
        |source: &Option<String>| source.as_deref().and_then(|text| Pattern::new(text).ok());
    if !report.from_meta {
        let tag = (task.record.get("tag").and_then(serde_json::Value::as_str))
            .filter(|tag| !tag.is_empty() && *tag != "null");
        let Some(tag) = tag else {
            return Vec::new();
        };
        let found = pattern(&report.tag_pattern)
            .and_then(|pattern| pattern.first_groups(tag).ok().flatten());
        let name = found.and_then(|groups| groups.into_iter().next().flatten());
        return vec![name.unwrap_or(tag.to_owned())];
    }
    let Some(meta) = task.values.iter().find_map(first_map) else {
        return Vec::new();
    };
    let key = pattern(&report.key_pattern);
    let entry = meta.iter().find(|(name, _)| {
        key.as_ref()
            .is_none_or(|pattern| pattern.find(name).unwrap_or(false))
    });
    match entry.map(|(_, value)| value) {
        Some(serde_json::Value::Array(items)) => (items.iter())
            .map(json_text)
            .filter(|name| !name.trim().is_empty())
            .collect(),
        Some(serde_json::Value::Null) | None => Vec::new(),
        Some(value) => vec![json_text(value)],
    }
}

fn first_map(value: &serde_json::Value) -> Option<&serde_json::Map<String, serde_json::Value>> {
    match value {
        serde_json::Value::Object(map) => Some(map),
        serde_json::Value::Array(items) => items.iter().find_map(first_map),
        _ => None,
    }
}

fn json_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// A `workflow_start` or `workflow_complete` event.
pub(crate) fn event(start: &Start, end: Option<&End>) -> Json {
    let text = |value: &str| Json::from(value);
    let optional = |value: &Option<String>| value.as_deref().map_or(Json::Null, Json::from);
    let script = std::fs::read(&start.script).ok();
    let script_id = script.map(|bytes| {
        let digest = Md5::digest(bytes);
        digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    });
    let name = std::path::Path::new(&start.script).file_name();
    let name = name.map(|name| name.to_string_lossy().into_owned());
    let mut event = Json::object([
        (
            "event_type",
            text(if end.is_some() {
                "workflow_complete"
            } else {
                "workflow_start"
            }),
        ),
        ("timestamp", Json::from(now())),
        ("run_name", text(&start.run_name)),
        ("script_id", Json::from(script_id)),
        ("script_name", Json::from(name)),
        ("command_line", text(&start.command_line)),
        ("params", Json::from(&start.params)),
        ("repository", optional(&start.repository)),
        ("commit_id", optional(&start.commit_id)),
        ("revision", optional(&start.revision)),
        ("start_time", Json::from(millis_time(Some(start.start)))),
    ]);
    if let Some(end) = end {
        let millis = (end.end - start.start).max(0);
        let duration = Json::object([
            ("durationInMillis", Json::from(millis)),
            ("days", Json::from(millis / 86_400_000)),
            ("hours", Json::from(millis / 3_600_000 % 24)),
            ("millis", Json::from(millis)),
            ("minutes", Json::from(millis / 60_000 % 60)),
            ("seconds", Json::from(millis / 1000 % 60)),
            (
                "units",
                Json::list(["DAYS", "HOURS", "MINUTES", "SECONDS", "MILLIS"]),
            ),
        ]);
        event.insert("end_time", millis_time(Some(end.end)));
        event.insert("duration", duration);
        event.insert("success", end.success);
        event.insert("exit_status", i64::from(!end.success));
        event.insert("error_message", end.error.clone());
        event.insert("error_report", end.error.clone());
    }
    let parts: Vec<&str> = NEXTFLOW_VERSION.split('.').collect();
    let version = Json::object([
        ("major", Json::from(parts[0])),
        ("minor", Json::from(parts[1])),
        ("patch", Json::from(parts[2])),
    ]);
    let launch = std::path::Path::new(&start.launch_dir);
    let output = start.config["outputDir"].as_str().unwrap_or("results");
    let variable = |name: &str| std::env::var(name).ok();
    for (key, value) in [
        ("nextflow_version", version),
        ("project_dir", text(&start.project_dir)),
        ("launch_dir", text(&start.launch_dir)),
        (
            "output_dir",
            Json::from(launch.join(output).to_string_lossy().into_owned()),
        ),
        ("work_dir", text(&start.work_dir)),
        ("home_dir", Json::from(variable("HOME"))),
        ("user_name", Json::from(variable("USER"))),
        ("profile", text(&start.profile)),
        ("session_id", text(&start.session_id)),
        ("container_engine", Json::Null),
        (
            "config_files",
            Json::list(start.config_files.iter().map(String::as_str)),
        ),
    ] {
        event.insert(key, value);
    }
    event
}

/// The TSV text of an execution event value: Nextflow's version and duration texts.
pub(crate) fn event_cell(key: &str, value: &Json) -> String {
    match (key, value) {
        ("nextflow_version", Json::Object(_)) => NEXTFLOW_VERSION.to_owned(),
        ("duration", Json::Object(_)) => {
            let millis = value
                .get("durationInMillis")
                .and_then(|millis| match millis {
                    Json::Number(number) => number.as_i64(),
                    _ => None,
                });
            Value::Duration(millis.unwrap_or(0)).to_string()
        }
        _ => super::files::cell(value),
    }
}

/// A size field of the trace as Nextflow's memory text, null when it is unknown.
fn size(task: &Finished, key: &str) -> Json {
    let bytes = task
        .task
        .record
        .get(key)
        .and_then(serde_json::Value::as_u64);
    Json::from(bytes.map(memory))
}

fn times(task: &Finished) -> [(&'static str, Json); 3] {
    [
        (
            "submit_time",
            Json::from(millis_time(task.number("submit"))),
        ),
        ("start_time", Json::from(millis_time(task.number("start")))),
        (
            "complete_time",
            Json::from(millis_time(task.number("complete"))),
        ),
    ]
}

fn files(task: &Finished) -> [(&'static str, Json); 7] {
    let outputs = Json::list(task.task.outputs.iter().map(|file| file.path.as_str()));
    let container = Json::from(task.text("container"));
    // uwe's executors keep task folders to themselves, so there is no work directory.
    [
        ("workdir", Json::Null),
        ("log", Json::Null),
        ("out", Json::Null),
        ("err", Json::Null),
        ("scratch", Json::Null),
        ("container", container),
        ("outputs", outputs),
    ]
}

fn cpu(task: &Finished) -> Json {
    let found = task
        .task
        .record
        .get("%cpu")
        .and_then(serde_json::Value::as_f64);
    Json::from(found)
}

/// A task of the task status report; `extra` goes after `exit_status`.
pub(crate) fn task_entry(task: &Finished, extra: Vec<(String, Json)>) -> Json {
    let mut entry = Json::object([
        ("task_id", Json::from(task.text("task_id"))),
        ("hash", Json::from(task.text("hash"))),
        ("process_name", Json::from(task.text("process"))),
        ("task_name", Json::from(task.text("name"))),
        ("tag", Json::from(task.text("tag"))),
        ("status", Json::from(task.trace_status())),
        ("exit_status", Json::from(task.number("exit"))),
    ]);
    for (key, value) in extra {
        entry.insert(&key, value);
    }
    for (key, value) in times(task) {
        entry.insert(key, value);
    }
    entry.insert("duration_ms", task.number("duration").unwrap_or(0));
    entry.insert("realtime_ms", task.number("realtime").unwrap_or(0));
    entry.insert("attempt", task.attempt());
    entry.insert("rchar", size(task, "rchar"));
    entry.insert("wchar", size(task, "wchar"));
    entry.insert("peak_rss", size(task, "peak_rss"));
    entry.insert("peak_vmem", size(task, "peak_vmem"));
    entry.insert("pct_cpu", cpu(task));
    for (key, value) in files(task) {
        entry.insert(key, value);
    }
    entry
}

/// A task of a sample of the sample status report, with the plugin's status.
pub(crate) fn sample_entry(task: &Finished, status: &str) -> Json {
    let mut entry = Json::object([
        ("task_id", Json::from(task.text("task_id"))),
        ("process_name", Json::from(task.text("process"))),
        ("task_name", Json::from(task.text("name"))),
        ("status", Json::from(status)),
    ]);
    for (key, value) in times(task) {
        entry.insert(key, value);
    }
    entry.insert("duration_ms", task.number("duration").unwrap_or(0));
    entry.insert("realtime_ms", task.number("realtime").unwrap_or(0));
    entry.insert("exit_status", Json::from(task.number("exit")));
    entry.insert("attempt", task.attempt());
    entry.insert("rchar", size(task, "rchar"));
    entry.insert("wchar", size(task, "wchar"));
    entry.insert("pct_cpu", cpu(task));
    entry.insert("peak_rss", size(task, "peak_rss"));
    entry.insert("peak_vmem", size(task, "peak_vmem"));
    for (key, value) in files(task) {
        entry.insert(key, value);
    }
    entry
}

/// An object with only the keys of `fields`, or all keys when `fields` names none.
pub(crate) fn filtered(entry: Json, fields: &[String]) -> Json {
    match entry {
        Json::Object(entries) if !fields.is_empty() => Json::Object(
            entries
                .into_iter()
                .filter(|(key, _)| fields.contains(key))
                .collect(),
        ),
        other => other,
    }
}

/// One sample of the sample status report, from its tasks and their statuses.
pub(crate) fn sample(name: &str, tasks: &[(&Finished, &'static str)], fields: &[String]) -> Json {
    let count = |status: &str| tasks.iter().filter(|(_, known)| *known == status).count() as i64;
    let done = count("COMPLETED") + count("CACHED");
    let failing = count("FAILED") + count("ABORTED") + count("IGNORED") > 0;
    let mut status = match (failing, done) {
        (true, 0) => "FAILED",
        (true, _) => "PARTIALLY_COMPLETED",
        (false, done) if done == tasks.len() as i64 => "COMPLETED",
        (false, _) => "PENDING",
    };
    let retried: Vec<&Finished> = (tasks.iter())
        .filter(|(_, status)| *status == "RETRIED")
        .map(|(task, _)| *task)
        .collect();
    let recovered = retried.iter().all(|failed| {
        tasks.iter().any(|(task, status)| {
            matches!(*status, "COMPLETED" | "CACHED")
                && task.text("name") == failed.text("name")
                && task.attempt() > failed.attempt()
        })
    });
    if !retried.is_empty() && recovered {
        status = if failing {
            "PARTIALLY_COMPLETED"
        } else {
            "COMPLETED"
        };
    }
    let starts = tasks.iter().filter_map(|(task, _)| task.number("start"));
    let completes = tasks.iter().filter_map(|(task, _)| task.number("complete"));
    let mut processes: Vec<String> = Vec::new();
    for (task, _) in tasks {
        if let Some(process) = task.text("process")
            && !processes.contains(&process)
        {
            processes.push(process);
        }
    }
    let entries = (tasks.iter()).map(|(task, status)| filtered(sample_entry(task, status), fields));
    Json::object([
        ("sample_id", Json::from(name)),
        ("status", Json::from(status)),
        ("tasks", Json::List(entries.collect())),
        (
            "task_counts",
            Json::object([
                ("total", Json::from(tasks.len() as i64)),
                ("completed", Json::from(count("COMPLETED"))),
                ("cached", Json::from(count("CACHED"))),
                ("failed", Json::from(count("FAILED"))),
                ("aborted", Json::from(count("ABORTED"))),
                ("failure_retried", Json::from(count("RETRIED"))),
                ("failure_ignored", Json::from(count("IGNORED"))),
            ]),
        ),
        (
            "first_task_start",
            Json::from(starts.min().map(|start| millis_time(Some(start)))),
        ),
        (
            "last_task_complete",
            Json::from(completes.max().map(|end| millis_time(Some(end)))),
        ),
        (
            "total_duration_ms",
            Json::from(
                tasks
                    .iter()
                    .filter_map(|(task, _)| task.number("duration"))
                    .sum::<i64>(),
            ),
        ),
        ("processes", Json::list(processes)),
    ])
}
