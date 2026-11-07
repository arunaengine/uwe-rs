//! nf-tencentcloud 2.0.1: writes `metadata.json` with the run and the events of its tasks. Its
//! executor runs on uwe's TES executor and its `cos://` paths on an S3 provider.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use jiff::Timestamp;
use nextflow_config::Config;
use nextflow_evaluator::value::Value;

use crate::catalogue::{Kind, Plugin, State};
use crate::observer::{Event, Observer, Task};
use crate::report_format::{Json, now};
use crate::run_metadata::Run;

pub(crate) const PLUGIN: Plugin = Plugin {
    scopes: &["tencentcloud", "tencent"],
    observer: Some(activate),
    ..Plugin::new("nf-tencentcloud", Kind::Executor, State::Partial)
};

/// The file of the metadata when the config names none, in the launch directory.
const FILE: &str = "metadata.json";

fn activate(config: &Config) -> Result<Option<Box<dyn Observer>>, String> {
    let setting = |key: &str| {
        (config.get(&["tencentcloud", "metadata", key]))
            .filter(|value| !matches!(value, Value::Null))
    };
    if !setting("enabled").is_some_and(|value| value.truth()) {
        return Ok(None);
    }
    let file = setting("file").map_or(FILE.to_owned(), |file| file.to_string());
    Ok(Some(Box::new(Metadata {
        file: PathBuf::from(file),
        run: None,
        trace: Vec::new(),
    })))
}

/// Keeps the run and the task events, and writes them all after each event.
struct Metadata {
    /// The file, relative to the launch directory unless absolute.
    file: PathBuf,
    run: Option<Run>,
    trace: Vec<Json>,
}

impl Observer for Metadata {
    fn notify(&mut self, event: &Event) {
        let name = match event {
            Event::RunStarted(start) => {
                self.file = Path::new(&start.launch_dir).join(&self.file);
                self.run = Some(Run::new(start));
                "started"
            }
            Event::TaskSubmitted(task) => self.task("process_submitted", task),
            Event::TaskStarted(task) => self.task("process_started", task),
            Event::TaskCompleted(task) => self.task("process_completed", task),
            Event::TaskFailed(task) => self.task("error", task),
            Event::RunEnded(end) => {
                if let Some(run) = &mut self.run {
                    run.end(end);
                }
                "completed"
            }
            Event::TaskCached(_) | Event::FilePublished { .. } => return,
        };
        self.write(name);
    }
}

impl Metadata {
    /// Adds the event of a task with its trace record, and gives its name.
    fn task(&mut self, name: &'static str, task: &Task) -> &'static str {
        let mut entry = self.head(name);
        entry.insert(
            "trace",
            Json::from(&serde_json::Value::Object(task.record.clone())),
        );
        self.trace.push(entry);
        name
    }

    /// The run name, run id, event and time in UTC that each entry starts with.
    fn head(&self, name: &str) -> Json {
        let start = self.run.as_ref().map(|run| &run.start);
        Json::object([
            (
                "runName",
                Json::from(start.map(|start| start.run_name.clone())),
            ),
            (
                "runId",
                Json::from(start.map(|start| start.session_id.clone())),
            ),
            ("event", Json::from(name)),
            ("utcTime", Json::from(utc(now()))),
        ])
    }

    /// Writes the whole file again: the latest event, the run and its params, and the trace.
    fn write(&self, name: &str) {
        let Some(run) = &self.run else {
            return;
        };
        let workflow = run.workflow(|millis| Json::from(utc(millis)));
        let metadata = Json::object([
            ("parameters", Json::from(&run.start.params)),
            ("workflow", workflow),
        ]);
        let document = (self.head(name))
            .with("metadata", metadata)
            .with("trace", Json::from(self.trace.clone()));
        if let Some(parent) = self.file.parent()
            && let Err(error) = std::fs::create_dir_all(parent)
        {
            return tracing::warn!(%error, "nf-tencentcloud cannot make {}", parent.display());
        }
        if let Err(error) = std::fs::write(&self.file, document.pretty()) {
            tracing::warn!(%error, "nf-tencentcloud cannot write {}", self.file.display());
        }
    }
}

/// A time in UTC in whole seconds, like `2026-11-06T11:48:43Z`.
fn utc(millis: i64) -> String {
    match Timestamp::from_millisecond(millis) {
        Ok(time) => time.strftime("%Y-%m-%dT%H:%M:%SZ").to_string(),
        Err(_) => String::new(),
    }
}
