//! nf-bigbrother 1.1.0: the graph of the tasks that ran, linked by the files they shared, as a
//! WfCommons WfFormat 1.6 snapshot with a Graphviz file, and an event log during the run.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use nextflow_config::Config;
use nextflow_evaluator::value::Value;

use crate::catalogue::{Kind, Plugin, State};
use crate::observer::{End, Event, Observer, Start, Task};
use crate::report_format::{Json, NEXTFLOW_VERSION, instant, now};

pub(crate) const PLUGIN: Plugin = Plugin {
    scopes: &["bigbrother"],
    observer: Some(activate),
    ..Plugin::new("nf-bigbrother", Kind::Observer, State::Implemented)
};

const DESCRIPTION: &str = "BigBrother physical execution trace";

/// The colours of the processes in the order the run meets them, again from the start.
const COLOURS: [&str; 12] = [
    "#8dd3c7", "#ffffb3", "#bebada", "#fb8072", "#80b1d3", "#fdb462", "#b3de69", "#fccde5",
    "#d9d9d9", "#bc80bd", "#ccebc5", "#ffed6f",
];

fn activate(config: &Config) -> Result<Option<Box<dyn Observer>>, String> {
    let get = |key: &str| {
        config
            .get(&["bigbrother", key])
            .filter(|value| !matches!(value, Value::Null))
    };
    let flag = |key: &str, default: bool| get(key).map_or(default, |value| value.truth());
    let launch = std::env::current_dir().map_err(|error| error.to_string())?;
    let folder =
        get("outputDir").map_or_else(|| "bigbrother".to_owned(), |value| value.to_string());
    let every = match get("snapshotEvery") {
        Some(Value::Number(number)) => number.to_f64() as usize,
        Some(other) => other.to_string().parse().unwrap_or(1),
        None => 1,
    };
    Ok(Some(Box::new(BigBrother {
        folder: launch.join(folder),
        events: flag("emitEvents", true),
        partials: flag("emitPartials", false),
        every: every.max(1),
        dot: flag("emitDot", true),
        prefix: get("prefix")
            .map(|value| value.to_string())
            .unwrap_or_default(),
        name: String::new(),
        session: String::new(),
        created: 0,
        tasks: Vec::new(),
        files: Vec::new(),
        finished: 0,
        snapshots: 0,
    })))
}

/// The physical graph of a run as its events come.
struct BigBrother {
    folder: PathBuf,
    events: bool,
    partials: bool,
    every: usize,
    dot: bool,
    prefix: String,
    /// The project name for file names, and the session id.
    name: String,
    session: String,
    /// The start of the run, in milliseconds since 1970.
    created: i64,
    tasks: Vec<Entry>,
    /// Each file with its size when the run first met it, `-1` when it could not be read.
    files: Vec<(String, i64)>,
    finished: usize,
    snapshots: usize,
}

/// A task of the graph.
struct Entry {
    id: String,
    name: String,
    inputs: Vec<String>,
    outputs: Vec<String>,
    /// When the run handed the task to its executor.
    pending: i64,
    execution: Option<Json>,
}

impl Observer for BigBrother {
    fn notify(&mut self, event: &Event) {
        match event {
            Event::RunStarted(start) => self.started(start),
            Event::TaskSubmitted(task) => {
                let inputs = self.expand(&task.inputs);
                let entry = self.entry(task);
                entry.inputs = inputs;
                entry.pending = now();
            }
            Event::TaskCompleted(task) | Event::TaskCached(task) | Event::TaskFailed(task) => {
                self.finish(task);
            }
            Event::RunEnded(end) => self.ended(end),
            Event::TaskStarted(_) | Event::FilePublished { .. } => {}
        }
    }
}

impl BigBrother {
    fn started(&mut self, start: &Start) {
        let name = match &start.repository {
            Some(_) => start.project_name.clone(),
            None => file_name(&start.script).to_owned(),
        };
        let name = if name.is_empty() {
            "workflow".to_owned()
        } else {
            name
        };
        self.name = (name.chars())
            .map(|character| match character {
                '/' | '\\' | '?' | '%' | '*' | ':' | '|' | '"' | '<' | '>' => '_',
                other if other.is_whitespace() => '_',
                other => other,
            })
            .collect();
        self.session = start.session_id.clone();
        self.created = start.start;
        if let Err(error) = std::fs::create_dir_all(&self.folder) {
            tracing::warn!(%error, "[BigBrother] {} was not made", self.folder.display());
        }
        tracing::info!(
            "[BigBrother] monitoring run '{}' -> {}",
            start.run_name,
            self.folder.display()
        );
        let mut line = Json::object([("event", Json::from("run"))]);
        for (key, value) in self.header() {
            if key == "createdAt" {
                line.insert("createdAt", value.clone());
                line.insert("executedAt", value);
            } else {
                line.insert(key, value);
            }
        }
        self.append(&line);
    }

    /// The entry of a task, made when the run first meets it.
    fn entry(&mut self, task: &Task) -> &mut Entry {
        let id = task.record["task_id"].to_string();
        let place = match self.tasks.iter().position(|entry| entry.id == id) {
            Some(place) => place,
            None => {
                self.tasks.push(Entry {
                    id,
                    name: text(&task.record["name"]),
                    inputs: Vec::new(),
                    outputs: Vec::new(),
                    pending: now(),
                    execution: None,
                });
                self.tasks.len() - 1
            }
        };
        &mut self.tasks[place]
    }

    fn finish(&mut self, task: &Task) {
        let inputs = self.expand(&task.inputs);
        // A failed task made no outputs that other tasks could read.
        let outputs = match task.record["status"] == "FAILED" {
            true => Vec::new(),
            false => self.expand(&task.outputs),
        };
        let entry = self.entry(task);
        entry.inputs = inputs;
        entry.outputs = outputs;
        let pending = entry.pending;
        entry.execution = Some(execution(task, pending));
        let id = entry.id.clone();
        let entry = &self.tasks[self
            .tasks
            .iter()
            .position(|entry| entry.id == id)
            .unwrap_or(0)];
        let line = Json::object([
            ("event", Json::from("task")),
            ("id", entry.id.as_str().into()),
            ("name", entry.name.as_str().into()),
            ("parents", Json::list(self.parents(entry))),
            ("inputFiles", Json::list(entry.inputs.iter())),
            ("outputFiles", Json::list(entry.outputs.iter())),
            ("execution", entry.execution.clone().unwrap_or(Json::Null)),
        ]);
        self.append(&line);
        self.finished += 1;
        if self.partials && self.finished.is_multiple_of(self.every) {
            let phase = format!("partial_{:03}", self.snapshots);
            self.snapshots += 1;
            self.snapshot(&phase, 0.0);
        }
    }

    fn ended(&mut self, end: &End) {
        let makespan = (end.end - self.created) as f64 / 1000.0;
        if !end.success {
            if let Some(task) = &end.task {
                let id = task.record["task_id"].to_string();
                if !self.tasks.iter().any(|entry| entry.id == id) {
                    self.finish(task);
                }
            }
            self.snapshot("error", makespan);
            self.append(&self.closing("error", makespan));
            tracing::warn!("[BigBrother] run failed, error snapshot written");
        }
        self.snapshot("complete", makespan);
        self.append(&self.closing("complete", makespan));
        tracing::info!(
            "[BigBrother] run complete: {} tasks recorded in {}",
            self.tasks.len(),
            self.folder.display()
        );
    }

    /// The local files of a task's files, with the regular files inside its folders, each
    /// once; the run remembers the size of each new one.
    fn expand(&mut self, files: &[crate::observer::File]) -> Vec<String> {
        let mut paths = Vec::new();
        for file in files {
            walk(Path::new(&file.path), &file.path, &mut paths);
        }
        let mut seen = BTreeSet::new();
        paths.retain(|path| seen.insert(path.clone()));
        for path in &paths {
            if !self.files.iter().any(|(known, _)| known == path) {
                let size = std::fs::metadata(path).map_or(-1, |metadata| metadata.len() as i64);
                self.files.push((path.clone(), size));
            }
        }
        paths
    }

    /// The tasks that made a file that the task read, by id in text order.
    fn parents(&self, entry: &Entry) -> Vec<String> {
        let mut parents: Vec<String> = (self.tasks.iter())
            .filter(|other| other.id != entry.id)
            .filter(|other| {
                other
                    .outputs
                    .iter()
                    .any(|output| entry.inputs.contains(output))
            })
            .map(|other| other.id.clone())
            .collect();
        parents.sort();
        parents
    }

    fn children(&self, entry: &Entry) -> Vec<String> {
        let mut children: Vec<String> = (self.tasks.iter())
            .filter(|other| other.id != entry.id)
            .filter(|other| {
                other
                    .inputs
                    .iter()
                    .any(|input| entry.outputs.contains(input))
            })
            .map(|other| other.id.clone())
            .collect();
        children.sort();
        children
    }

    fn header(&self) -> Vec<(&'static str, Json)> {
        vec![
            ("name", Json::from(self.name.as_str())),
            ("description", DESCRIPTION.into()),
            ("createdAt", instant(self.created).into()),
            ("schemaVersion", "1.6".into()),
            (
                "runtimeSystem",
                Json::object([
                    ("name", Json::from("Nextflow")),
                    ("url", "https://www.nextflow.io/".into()),
                    ("version", NEXTFLOW_VERSION.into()),
                ]),
            ),
        ]
    }

    fn file_list(&self) -> Json {
        Json::list(self.files.iter().map(|(path, size)| {
            Json::object([
                ("id", Json::from(path.as_str())),
                ("sizeInBytes", (*size).into()),
            ])
        }))
    }

    /// The last line of the event log: the end of the run or its failure.
    fn closing(&self, phase: &str, makespan: f64) -> Json {
        Json::object([
            ("event", Json::from(phase)),
            ("makespanInSeconds", makespan.into()),
            ("taskCount", (self.tasks.len() as u64).into()),
            ("files", self.file_list()),
            ("machines", Json::List(Vec::new())),
        ])
    }

    /// Writes the snapshot of a phase, like `complete` or `partial_000`, and its graph.
    fn snapshot(&self, phase: &str, makespan: f64) {
        let tasks = self.tasks.iter().map(|entry| {
            Json::object([
                ("name", Json::from(entry.name.as_str())),
                ("id", entry.id.as_str().into()),
                ("parents", Json::list(self.parents(entry))),
                ("children", Json::list(self.children(entry))),
                ("inputFiles", Json::list(entry.inputs.iter())),
                ("outputFiles", Json::list(entry.outputs.iter())),
            ])
        });
        let executions = (self.tasks.iter()).map(|entry| {
            entry
                .execution
                .clone()
                .unwrap_or_else(|| execution_default(&entry.id))
        });
        let mut document = Json::Object(Vec::new());
        for (key, value) in self.header() {
            document.insert(key, value);
        }
        document.insert(
            "workflow",
            Json::object([
                (
                    "specification",
                    Json::object([("tasks", Json::list(tasks)), ("files", self.file_list())]),
                ),
                (
                    "execution",
                    Json::object([
                        ("makespanInSeconds", makespan.into()),
                        ("executedAt", instant(self.created).into()),
                        ("tasks", Json::list(executions)),
                    ]),
                ),
            ]),
        );
        let base = self.file(phase);
        if let Err(error) =
            std::fs::write(self.folder.join(format!("{base}.json")), document.pretty())
        {
            tracing::warn!("[BigBrother] failed to write snapshot {base}: {error}");
        }
        if self.dot
            && let Err(error) =
                std::fs::write(self.folder.join(format!("{base}.dot")), self.graph())
        {
            tracing::warn!("[BigBrother] failed to write snapshot {base}: {error}");
        }
    }

    /// The Graphviz text of the graph, a box per task coloured by its process.
    fn graph(&self) -> String {
        let mut text = String::from(
            "digraph physical {\n  rankdir=TB;\n  node [shape=box, style=\"rounded,filled\", \
             fontname=\"Helvetica\", fontsize=10];\n  edge [color=\"#666666\"];\n",
        );
        let mut processes: Vec<String> = Vec::new();
        for entry in &self.tasks {
            let label = label(&entry.name);
            let place = match processes.iter().position(|known| *known == label) {
                Some(place) => place,
                None => {
                    processes.push(label.clone());
                    processes.len() - 1
                }
            };
            let escaped = label.replace('\\', "\\\\").replace('"', "\\\"");
            text.push_str(&format!(
                "  \"t{}\" [label=\"{escaped}\\\\n[{}]\", fillcolor=\"{}\"];\n",
                entry.id,
                entry.id,
                COLOURS[place % COLOURS.len()]
            ));
        }
        for entry in &self.tasks {
            for child in self.children(entry) {
                text.push_str(&format!("  \"t{}\" -> \"t{child}\";\n", entry.id));
            }
        }
        text.push_str("}\n");
        text
    }

    /// The name of a file: `[<prefix>_]<phase>_<name>_<session>`.
    fn file(&self, phase: &str) -> String {
        let base = format!("{phase}_{}_{}", self.name, self.session);
        match self.prefix.is_empty() {
            true => base,
            false => format!("{}_{base}", self.prefix),
        }
    }

    fn append(&self, line: &Json) {
        if !self.events {
            return;
        }
        let path = self.folder.join(format!("{}.jsonl", self.file("events")));
        let written = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut file| writeln!(file, "{}", line.compact()));
        if let Err(error) = written {
            tracing::warn!(
                "[BigBrother] failed to append to {}: {error}",
                path.display()
            );
        }
    }
}

/// The local regular files of a path, the files below a folder in name order; a path that is
/// not on this machine as it is.
fn walk(path: &Path, text: &str, into: &mut Vec<String>) {
    if !path.is_dir() {
        into.push(text.to_owned());
        return;
    }
    let mut entries: Vec<PathBuf> = (std::fs::read_dir(path).into_iter().flatten())
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();
    entries.sort();
    for entry in entries {
        let text = entry.to_string_lossy().into_owned();
        walk(&entry, &text, into);
    }
}

/// The execution record of a task.
fn execution(task: &Task, pending: i64) -> Json {
    let _ = (task, pending);
    Json::Object(Vec::new())
}

/// The execution record of a task that never finished: defaults throughout.
fn execution_default(id: &str) -> Json {
    let mut record = crate::observer::Record::new();
    record.insert(
        "task_id".into(),
        id.parse::<i64>().map_or_else(|_| id.into(), Into::into),
    );
    let task = Task {
        record,
        process_id: None,
        hash: String::new(),
        executor: String::new(),
        inputs: Vec::new(),
        outputs: Vec::new(),
        values: Vec::new(),
    };
    let mut json = execution(&task, 0);
    json.insert("id", Json::from(id));
    json
}

/// A trace value as text: a text as it is, a number as its digits, null as nothing.
fn text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => String::new(),
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// The label of a task: its name without a final ` (...)` and without the workflows before
/// the last `:`; `task` when nothing is left.
fn label(name: &str) -> String {
    let name = match name.rfind(" (") {
        Some(place) if name.ends_with(')') => &name[..place],
        _ => name,
    };
    let name = name.rsplit(':').next().unwrap_or(name);
    if name.is_empty() {
        "task".to_owned()
    } else {
        name.to_owned()
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_tasks() {
        assert_eq!(label("SHOUT (3)"), "SHOUT");
        assert_eq!(label("NF:SUB:ALIGN (s1)"), "ALIGN");
        assert_eq!(label(""), "task");
    }
}
