//! nf-datatrail 0.0.1: records the files that each task read and wrote, as CSV lists while
//! the tasks run and as a per-task summary at the end of the run.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use nextflow_config::Config;
use nextflow_evaluator::value::Value;

use crate::catalogue::{Kind, Plugin, State};
use crate::observer::{Event, Observer, Task};

pub(crate) const PLUGIN: Plugin = Plugin {
    scopes: &["datatrail"],
    observer: Some(activate),
    ..Plugin::new("nf-datatrail", Kind::Observer, State::Partial)
};

/// What the `datatrail` scope asks for.
struct Settings {
    input: Option<String>,
    output: Option<String>,
    summary: Option<String>,
    delimiter: String,
}

fn activate(config: &Config) -> Result<Option<Box<dyn Observer>>, String> {
    let launch = std::env::current_dir().map_err(|error| error.to_string())?;
    let Some(settings) = Settings::read(config, &launch)? else {
        return Ok(None);
    };
    let csv = |path: &Option<String>| -> Result<Option<File>, String> {
        let Some(path) = path else {
            return Ok(None);
        };
        let mut file = create(&launch.join(path)).map_err(|error| error.to_string())?;
        let header = ["name", "hash", "path", "type", "size"].join(&settings.delimiter);
        writeln!(file, "{header}").map_err(|error| error.to_string())?;
        Ok(Some(file))
    };
    let (inputs, outputs) = (csv(&settings.input)?, csv(&settings.output)?);
    Ok(Some(Box::new(Datatrail {
        launch,
        settings,
        inputs,
        outputs,
        tasks: Vec::new(),
        made: HashMap::new(),
        ended: false,
    })))
}

impl Settings {
    /// The settings, `None` when no output is asked for; a file that is there already stops
    /// the run unless `overwrite` is set.
    fn read(config: &Config, launch: &Path) -> Result<Option<Settings>, String> {
        let get = |path: &[&str]| {
            let mut full = vec!["datatrail"];
            full.extend_from_slice(path);
            config
                .get(&full)
                .filter(|value| !matches!(value, Value::Null))
        };
        let text = |path: &[&str]| {
            get(path)
                .map(|value| value.to_string())
                .filter(|t| !t.is_empty())
        };
        let flag = |path: &[&str], default: bool| match get(path) {
            None => default,
            Some(Value::Boolean(flag)) => flag,
            Some(value) => match value.to_string().to_lowercase().as_str() {
                "true" | "yes" | "1" => true,
                "false" | "no" | "0" => false,
                other => {
                    tracing::warn!(
                        "Invalid boolean value: {other} -- using default value: {default}"
                    );
                    default
                }
            },
        };
        let overwrite = flag(&["overwrite"], false);
        let mut lists = Vec::new();
        for (key, name) in [
            ("input", "Input"),
            ("output", "Output"),
            ("summary", "Summary"),
        ] {
            let path = text(&[key]);
            if let Some(path) = &path {
                if !path.ends_with(".csv") {
                    return Err(format!("{name} file must be a CSV file: {path}"));
                }
                prepare(&launch.join(path), path, name, overwrite)?;
            }
            lists.push(path);
        }
        if lists.iter().all(Option::is_none) {
            return Ok(None);
        }
        let summary = lists.pop().flatten();
        let output = lists.pop().flatten();
        let input = lists.pop().flatten();
        Ok(Some(Settings {
            input,
            output,
            summary,
            delimiter: text(&["delimiter"]).unwrap_or_else(|| ";".to_owned()),
        }))
    }
}

/// Checks that a file may be written, and removes the one there with `overwrite`.
fn prepare(path: &Path, shown: &str, name: &str, overwrite: bool) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    let kind = if name == "DAG" {
        "DAG".to_owned()
    } else {
        name.to_lowercase()
    };
    if !overwrite {
        return Err(format!(
            "{name} file already exists: {shown} -- enable `dag.overwrite` in your config file \
             to overwrite existing {kind} files"
        ));
    }
    if path.is_dir() || std::fs::remove_file(path).is_err() {
        return Err(format!("Unable to overwrite existing {name} file: {shown}"));
    }
    Ok(())
}

/// A file, made with its folders.
fn create(path: &Path) -> std::io::Result<File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    File::create(path)
}

/// The run observer, which writes the lists as the tasks run and the rest at the end.
struct Datatrail {
    launch: PathBuf,
    settings: Settings,
    inputs: Option<File>,
    outputs: Option<File>,
    tasks: Vec<Record>,
    /// The task that made each output path, by its place in `tasks`.
    made: HashMap<String, usize>,
    ended: bool,
}

/// A task with the files it read and wrote.
struct Record {
    id: i64,
    name: String,
    hash: String,
    inputs: Vec<Entry>,
    outputs: Vec<Entry>,
    /// Files and bytes read from each earlier task, in the order first read.
    producers: Vec<(usize, u64, u64)>,
    /// Files and bytes that no task made.
    external: (u64, u64),
}

/// A file or folder with its size, folders with all files below them.
struct Entry {
    path: String,
    folder: bool,
    size: u64,
}

impl Observer for Datatrail {
    fn notify(&mut self, event: &Event) {
        match event {
            // A cached task does not start, so only the tasks that run appear, as in Nextflow.
            Event::TaskStarted(task) => self.started(task),
            Event::TaskCompleted(task) | Event::TaskFailed(task) => self.completed(task),
            Event::RunEnded(_) if !std::mem::replace(&mut self.ended, true) => self.finish(),
            _ => {}
        }
    }
}

impl Datatrail {
    fn started(&mut self, task: &Task) {
        let text = |name: &str| match task.record.get(name) {
            Some(serde_json::Value::String(text)) => Some(text.clone()),
            Some(serde_json::Value::Null) | None => None,
            Some(other) => Some(other.to_string()),
        };
        let mut record = Record {
            id: task
                .record
                .get("task_id")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0),
            name: text("name").unwrap_or_default(),
            hash: task.hash.clone(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            producers: Vec::new(),
            external: (0, 0),
        };
        let mut seen = BTreeSet::new();
        for input in task
            .inputs
            .iter()
            .filter(|input| seen.insert(input.path.clone()))
        {
            let entry = entry(&input.path);
            let producer = ancestors(&entry.path).find_map(|path| self.made.get(path).copied());
            match producer {
                Some(producer) => match record
                    .producers
                    .iter_mut()
                    .find(|known| known.0 == producer)
                {
                    Some(known) => (known.1, known.2) = (known.1 + 1, known.2 + entry.size),
                    None => record.producers.push((producer, 1, entry.size)),
                },
                None => record.external = (record.external.0 + 1, record.external.1 + entry.size),
            }
            line(&mut self.inputs, &record, &entry, &self.settings.delimiter);
            record.inputs.push(entry);
        }
        self.tasks.push(record);
    }

    fn completed(&mut self, task: &Task) {
        let Some(place) = self
            .tasks
            .iter()
            .rposition(|record| record.hash == task.hash)
        else {
            return;
        };
        let mut seen = BTreeSet::new();
        for output in task
            .outputs
            .iter()
            .filter(|output| seen.insert(output.path.clone()))
        {
            let entry = entry(&output.path);
            self.made.insert(entry.path.clone(), place);
            line(
                &mut self.outputs,
                &self.tasks[place],
                &entry,
                &self.settings.delimiter,
            );
            self.tasks[place].outputs.push(entry);
        }
    }

    /// Writes the summary, and closes the lists.
    fn finish(&mut self) {
        self.inputs = None;
        self.outputs = None;
        if let Some(summary) = &self.settings.summary {
            write(&self.launch.join(summary), &self.summary());
        }
    }

    /// One line per task by task id, with the number of later tasks that read its outputs.
    fn summary(&self) -> String {
        let delimiter = &self.settings.delimiter;
        let header = [
            "task",
            "hash",
            "inputs",
            "inputSize",
            "outputs",
            "outputSize",
            "usedBy",
        ];
        let mut text = header.join(delimiter) + "\n";
        let mut order: Vec<usize> = (0..self.tasks.len()).collect();
        order.sort_by_key(|place| self.tasks[*place].id);
        for place in order {
            let task = &self.tasks[place];
            let used = (self.tasks.iter())
                .filter(|other| other.producers.iter().any(|producer| producer.0 == place))
                .count();
            let total = |entries: &[Entry]| entries.iter().map(|entry| entry.size).sum::<u64>();
            let fields = [
                task.name.clone(),
                task.hash.clone(),
                task.inputs.len().to_string(),
                total(&task.inputs).to_string(),
                task.outputs.len().to_string(),
                total(&task.outputs).to_string(),
                used.to_string(),
            ];
            text.push_str(&fields.join(delimiter));
            text.push('\n');
        }
        text
    }
}

/// Appends the line of a file to a list.
fn line(file: &mut Option<File>, task: &Record, entry: &Entry, delimiter: &str) {
    let Some(file) = file else {
        return;
    };
    let kind = if entry.folder { "d" } else { "f" };
    let size = entry.size.to_string();
    let fields = [task.name.as_str(), &task.hash, &entry.path, kind, &size];
    if let Err(error) = writeln!(file, "{}", fields.join(delimiter)) {
        tracing::warn!("nf-datatrail: could not write a file list: {error}");
    }
}

fn write(path: &Path, text: &str) {
    if let Err(error) = create(path).and_then(|mut file| file.write_all(text.as_bytes())) {
        tracing::warn!("nf-datatrail: could not write {}: {error}", path.display());
    }
}

/// A path with its size; a folder holds the size of all files below it. A remote address
/// counts as empty, since uwe does not ask its store.
fn entry(path: &str) -> Entry {
    let local = Path::new(path);
    let folder = local.is_dir();
    let size = match folder {
        true => folder_size(local),
        false => std::fs::metadata(local).map_or(0, |metadata| metadata.len()),
    };
    Entry {
        path: path.to_owned(),
        folder,
        size,
    }
}

fn folder_size(folder: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return 0;
    };
    (entries.flatten())
        .map(|entry| match entry.path().is_dir() {
            true => folder_size(&entry.path()),
            false => entry.metadata().map_or(0, |metadata| metadata.len()),
        })
        .sum()
}

/// A path and the folders above it, nearest first.
fn ancestors(path: &str) -> impl Iterator<Item = &str> {
    std::iter::successors(Some(path), |path| {
        path.rsplit_once('/')
            .map(|(parent, _)| parent)
            .filter(|parent| !parent.is_empty())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_ancestors() {
        assert_eq!(
            ancestors("/a/b/c").collect::<Vec<_>>(),
            ["/a/b/c", "/a/b", "/a"]
        );
    }
}
