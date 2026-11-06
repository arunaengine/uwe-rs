//! The progress dashboard of nf-seqlab-progress: task states from the run's events, and the
//! stages, files and publish counts they give, for the console and the audit file.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::Write as _;
use std::path::PathBuf;
use std::time::Instant;

use super::registry::{Boundary, Registry};
use crate::observer::{Event, Observer, Start, Task};
use crate::report_format::Json;

/// How the dashboard is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    /// uwe's text view, for a terminal.
    View,
    Plain,
    Json,
    Off,
}

/// A task as the dashboard knows it.
#[derive(Clone, Debug)]
struct Tracked {
    id: String,
    process: String,
    file: String,
    parent: String,
    attempt: i64,
    state: &'static str,
}

/// What the dashboard counts of the published files.
#[derive(Clone, Debug)]
struct Published {
    files: u64,
    bytes: u64,
    last: String,
    when: Instant,
}

pub(crate) struct Board {
    mode: Mode,
    limit: usize,
    /// The output folder of `params.outdir`, which holds `pipeline_info/progress.jsonl`.
    outdir: String,
    audit: Option<PathBuf>,
    run: String,
    registry: Registry,
    tasks: Vec<Tracked>,
    /// The parent files that a boundary task completed, by stage.
    completed: Vec<(String, String)>,
    published: Option<Published>,
    last: Option<String>,
}

impl Board {
    pub(crate) fn new(mode: Mode, limit: usize, outdir: String) -> Board {
        Board {
            mode,
            limit,
            outdir,
            audit: None,
            run: String::new(),
            registry: Registry::default(),
            tasks: Vec::new(),
            completed: Vec::new(),
            published: None,
            last: None,
        }
    }

    fn started(&mut self, start: &Start) {
        self.run = [&start.run_name, &start.session_id]
            .into_iter()
            .find(|name| !name.is_empty())
            .cloned()
            .unwrap_or("nf-seqlab".to_owned());
        self.registry = super::registry::snapshot();
        let outdir = self.outdir.strip_prefix("file://").unwrap_or(&self.outdir);
        match outdir.contains("://") {
            true => tracing::warn!(
                "Unable to create nf-seqlab progress audit: uwe writes it only to a local folder, \
                 not {outdir}"
            ),
            false => {
                let folder = PathBuf::from(&start.launch_dir).join(outdir);
                self.audit = Some(folder.join("pipeline_info/progress.jsonl"));
            }
        }
    }

    /// The file, parent, process, task id and attempt of a task, from its meta map.
    fn identity(&self, task: &Task) -> Option<Tracked> {
        let meta = task.values.iter().find_map(first_map)?;
        let text = |key: &str| {
            meta.get(key)
                .filter(|value| !value.is_null())
                .map(json_text)
        };
        let file = text("file_id").or_else(|| text("id"))?;
        let parent = (text("parent_file_id").or_else(|| text("parent_id"))).unwrap_or(file.clone());
        let record = |key: &str| {
            task.record
                .get(key)
                .filter(|value| !value.is_null())
                .map(json_text)
        };
        let process = record("process")
            .or_else(|| record("name"))
            .unwrap_or_default();
        let process = process.rsplit(':').next().unwrap_or_default().to_owned();
        Some(Tracked {
            id: record("task_id").unwrap_or_default(),
            process,
            file,
            parent,
            attempt: task
                .record
                .get("attempt")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(1),
            state: "pending",
        })
    }

    /// Takes a task event; a refused one only warns.
    fn task(&mut self, task: &Task, state: &'static str, kind: &str) {
        let Some(mut tracked) = self.identity(task) else {
            return;
        };
        if let Err(reason) = self.apply(&mut tracked, task, state) {
            tracing::warn!(
                "nf-seqlab progress: ignoring {kind} event for {} (file={} parent={} task={} \
                 attempt={}): {reason}",
                tracked.process,
                tracked.file,
                tracked.parent,
                tracked.id,
                tracked.attempt
            );
        }
    }

    fn apply(
        &mut self,
        tracked: &mut Tracked,
        task: &Task,
        state: &'static str,
    ) -> Result<(), String> {
        let mapping = (self.registry.mappings.iter())
            .find(|mapping| mapping.process == tracked.process)
            .cloned()
            .ok_or_else(|| format!("Unmapped process '{}'", tracked.process))?;
        if !self
            .registry
            .inputs
            .iter()
            .any(|(id, _)| *id == tracked.parent)
        {
            return Err(format!("Unknown source file '{}'", tracked.parent));
        }
        if let Some(known) = self.tasks.iter().find(|known| known.id == tracked.id)
            && known.attempt > tracked.attempt
        {
            return Err(format!("Stale attempt for task '{}'", tracked.id));
        }
        let success = matches!(
            task.record
                .get("status")
                .and_then(serde_json::Value::as_str),
            Some("COMPLETED" | "SUCCEEDED") | None
        );
        tracked.state = match state {
            "completed" if !success => "failed",
            other => other,
        };
        let boundary = match mapping.boundary {
            Boundary::Always => true,
            Boundary::Never => false,
            Boundary::Parent => tracked.file == tracked.parent,
        };
        if matches!(tracked.state, "completed" | "cached") && boundary {
            let stage = self
                .registry
                .stages
                .iter()
                .find(|stage| stage.id == mapping.stage);
            if !stage.is_some_and(|stage| stage.files.contains(&tracked.parent)) {
                return Err(format!(
                    "Source file '{}' is not applicable to stage '{}'",
                    tracked.parent, mapping.stage
                ));
            }
            let done = (mapping.stage.clone(), tracked.parent.clone());
            if !self.completed.contains(&done) {
                self.completed.push(done);
            }
        }
        // A newer attempt replaces the older ones of the same task.
        match self.tasks.iter_mut().find(|known| known.id == tracked.id) {
            Some(known) => *known = tracked.clone(),
            None => self.tasks.push(tracked.clone()),
        }
        Ok(())
    }

    fn stage_of(&self, process: &str) -> Option<&str> {
        let mapping = self
            .registry
            .mappings
            .iter()
            .find(|mapping| mapping.process == process);
        mapping.map(|mapping| mapping.stage.as_str())
    }

    /// The dashboard state as its JSON line.
    pub(crate) fn dashboard(&self) -> Json {
        let mut stages = Vec::new();
        for stage in &self.registry.stages {
            let tasks: Vec<&Tracked> = (self.tasks.iter())
                .filter(|task| self.stage_of(&task.process) == Some(stage.id.as_str()))
                .collect();
            let done = (stage.files.iter())
                .filter(|file| {
                    self.completed
                        .contains(&(stage.id.clone(), (*file).clone()))
                })
                .count();
            let expected = stage.files.len();
            let state = if tasks.iter().any(|task| task.state == "failed") {
                "failed"
            } else if expected > 0 && done == expected {
                "completed"
            } else if !tasks.is_empty() {
                "running"
            } else {
                "queued"
            };
            let percent = match expected {
                0 => 0.0,
                _ => 100.0 * done as f64 / expected as f64,
            };
            stages.push(Json::object([
                ("id", Json::from(stage.id.as_str())),
                ("label", Json::from(stage.label.as_str())),
                ("state", Json::from(state)),
                ("completed_files", Json::from(done as i64)),
                ("expected_files", Json::from(expected as i64)),
                ("percent", Json::from(percent)),
            ]));
        }
        let state = |stage: &Json| {
            stage
                .get("state")
                .and_then(Json::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let current = (stages
            .iter()
            .position(|stage| matches!(state(stage).as_str(), "running" | "failed")))
        .or_else(|| stages.iter().position(|stage| state(stage) == "queued"))
        .or_else(|| stages.len().checked_sub(1));
        let mut active: Vec<Json> = Vec::new();
        if let Some(current) = current {
            let stage = &self.registry.stages[current];
            let mut seen: Vec<&str> = Vec::new();
            for task in &self.tasks {
                let busy = matches!(task.state, "pending" | "submitted" | "running");
                let done = self
                    .completed
                    .contains(&(stage.id.clone(), task.parent.clone()));
                if busy
                    && !done
                    && self.stage_of(&task.process) == Some(stage.id.as_str())
                    && !seen.contains(&task.parent.as_str())
                    && seen.len() < self.limit
                {
                    seen.push(&task.parent);
                }
            }
            active = (seen.into_iter())
                .map(|file| {
                    Json::object([
                        ("file_id", Json::from(file)),
                        ("phase", Json::Null),
                        ("state", Json::from("running")),
                        ("completed", Json::from(0i64)),
                        ("total", Json::Null),
                        ("unit", Json::Null),
                        ("percent", Json::Null),
                    ])
                })
                .collect();
        }
        let errors = self
            .tasks
            .iter()
            .filter(|task| task.state == "failed")
            .count();
        let publish = self.published.as_ref().map(|published| {
            Json::object([
                ("completed_files", Json::from(published.files)),
                ("completed_bytes", Json::from(published.bytes)),
                ("in_flight", Json::Null),
                ("last_target", Json::from(safe(&published.last))),
                (
                    "last_age_seconds",
                    Json::from(published.when.elapsed().as_secs()),
                ),
            ])
        });
        Json::object([
            ("schema", Json::from("nf-seqlab.dashboard/v1")),
            ("run_id", Json::from(safe(&self.run))),
            ("run_name", Json::from(safe(&self.run))),
            (
                "stage",
                current.map_or(Json::Null, |current| stages[current].clone()),
            ),
            ("stages", Json::List(stages)),
            ("active_files", Json::List(active)),
            ("error_count", Json::from(errors as i64)),
            ("publish", Json::from(publish)),
        ])
    }

    /// Writes the state to the audit file, and shows it when it changed.
    fn show(&mut self) {
        let board = self.dashboard();
        let line = board.compact();
        if let Some(audit) = &self.audit {
            let appended = std::fs::create_dir_all(audit.parent().unwrap_or(audit))
                .and_then(|()| {
                    std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(audit)
                })
                .and_then(|mut file| writeln!(file, "{line}"));
            if let Err(error) = appended {
                tracing::warn!("Unable to append nf-seqlab progress audit: {error}");
            }
        }
        let shown = match self.mode {
            Mode::Off => return,
            Mode::Plain => super::view::plain(&board),
            Mode::Json => line,
            Mode::View => super::view::text(&board),
        };
        if self.last.as_ref() != Some(&shown) {
            // The dashboard is the plugin's output on the console, as Nextflow's own lines are.
            println!("{}", shown.trim_end());
            self.last = Some(shown);
        }
    }
}

impl Observer for Board {
    fn notify(&mut self, event: &Event) {
        match event {
            Event::RunStarted(start) => self.started(start),
            Event::TaskSubmitted(task) => self.task(task, "submitted", "submit"),
            Event::TaskStarted(task) => self.task(task, "running", "start"),
            Event::TaskCompleted(task) => self.task(task, "completed", "complete"),
            Event::TaskCached(task) => self.task(task, "cached", "cached"),
            Event::TaskFailed(task) => self.task(task, "failed", "error"),
            Event::FilePublished { source, target } => {
                let size = std::fs::metadata(source).map_or(0, |found| match found.is_dir() {
                    true => folder_size(source),
                    false => found.len(),
                });
                let published = self.published.get_or_insert(Published {
                    files: 0,
                    bytes: 0,
                    last: String::new(),
                    when: Instant::now(),
                });
                published.files += 1;
                published.bytes += size;
                published.last = target.clone();
                published.when = Instant::now();
            }
            Event::RunEnded(_) => {}
        }
        self.show();
    }
}

/// The bytes of the regular files under a folder.
fn folder_size(folder: &str) -> u64 {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return 0;
    };
    (entries.flatten())
        .map(|entry| match entry.metadata() {
            Ok(found) if found.is_dir() => folder_size(&entry.path().to_string_lossy()),
            Ok(found) => found.len(),
            Err(_) => 0,
        })
        .sum()
}

/// Text without control characters, which become spaces.
fn safe(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
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

#[cfg(test)]
mod tests {
    use super::super::registry::{Mapping, Stage};
    use super::*;

    fn task(status: &str) -> Task {
        let record = serde_json::json!({
            "task_id": 1, "process": "EXACT_PROGRESS", "attempt": 1, "status": status
        });
        Task {
            record: record.as_object().cloned().unwrap_or_default(),
            process_id: Some(1),
            hash: String::new(),
            executor: "local".into(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            values: vec![serde_json::json!({ "file_id": "chr22", "parent_file_id": "chr22" })],
            declared: Vec::new(),
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    #[test]
    fn prints_recorded_lines() {
        let mut board = Board::new(Mode::Off, 2, "results".into());
        board.run = "progress-success".into();
        board.registry = Registry {
            inputs: vec![("chr22".into(), "synthetic://chr22".into())],
            stages: vec![Stage {
                id: "exact".into(),
                label: "Exact progress".into(),
                files: vec!["chr22".into()],
            }],
            mappings: vec![Mapping {
                process: "EXACT_PROGRESS".into(),
                stage: "exact".into(),
                boundary: Boundary::Parent,
            }],
        };
        let line = |board: &Board| super::super::view::plain(&board.dashboard());
        let prefix = "[nf-seqlab progress] run=progress-success stage=exact files=";
        board.notify(&Event::TaskSubmitted(task("SUBMITTED")));
        assert_eq!(
            line(&board),
            format!(
                "{prefix}0/1 stage_percent=0.0 state=running file=chr22 file_percent=indeterminate phase=running"
            )
        );
        board.notify(&Event::TaskCompleted(task("COMPLETED")));
        assert_eq!(
            line(&board),
            format!("{prefix}1/1 stage_percent=100.0 state=completed")
        );
        let failed = task("FAILED");
        board.notify(&Event::TaskFailed(failed));
        assert_eq!(
            board.dashboard().get("error_count"),
            Some(&Json::from(1i64))
        );
    }
}
