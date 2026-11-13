//! nf-storagemetrics 0.1.1: the size of each finished task's folder, one tab-separated line per
//! task, and a summary in the log at the end of the run.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fs::File;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use nextflow_config::Config;
use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::value::Value;

use crate::Context;
use crate::catalogue::{Kind, Plugin, State};
use crate::observer::{Event, Observer, Start, Task};

pub(crate) const PLUGIN: Plugin = Plugin {
    functions: &["sayHello"],
    call: Some(say_hello),
    scopes: &["storagemetrics"],
    observer: Some(activate),
    ..Plugin::new("nf-storagemetrics", Kind::Observer, State::Partial)
};

const HEADER: &str = "task_id\thash\tprocess\ttag\tstatus\tworkdir\tsize_bytes\tsize_human\n";

/// `sayHello(target)`, the plugin's example function: prints a greeting of the target.
pub(crate) fn say_hello(
    name: &str,
    arguments: Vec<Value>,
    context: &mut Context,
) -> Result<Value, Error> {
    let [target] = arguments.as_slice() else {
        return Err(Error::Unsupported {
            message: format!("the function `{name}` with these arguments"),
            span: Default::default(),
        });
    };
    context.host.print(&format!("Hello, {target}!\n"));
    Ok(Value::Null)
}

fn activate(
    config: &Config,
    _: &std::path::Path,
    _: &crate::catalogue::Environment,
) -> Result<Option<Box<dyn Observer>>, String> {
    let file = config.get(&["storagemetrics", "file"]);
    let file = file
        .filter(|value| !matches!(value, Value::Null))
        .map(|value| value.to_string());
    Ok(Some(Box::new(Metrics {
        file,
        path: PathBuf::new(),
        output: None,
        header: false,
        count: 0,
        total: 0,
    })))
}

/// The lines of the run and its totals.
struct Metrics {
    /// The file as the config gives it; by default named after the run and its session.
    file: Option<String>,
    path: PathBuf,
    output: Option<File>,
    header: bool,
    count: u64,
    total: u64,
}

impl Observer for Metrics {
    fn notify(&mut self, event: &Event) {
        match event {
            Event::RunStarted(start) => self.started(start),
            Event::TaskCompleted(task) | Event::TaskFailed(task) => self.measure(task),
            Event::RunEnded(_) => self.ended(),
            _ => {}
        }
    }
}

impl Metrics {
    /// Makes the file empty, below the parent of the work directory.
    fn started(&mut self, start: &Start) {
        let session: String = start.session_id.chars().take(8).collect();
        let name = (self.file.clone())
            .unwrap_or_else(|| format!("storage-metrics.{}.{session}.txt", start.run_name));
        let base = Path::new(&start.work_dir)
            .parent()
            .unwrap_or(Path::new(&start.launch_dir));
        self.path = base.join(&name);
        self.file = Some(name.clone());
        let parent = self.path.parent().map(std::fs::create_dir_all);
        match parent
            .unwrap_or(Ok(()))
            .and_then(|()| File::create(&self.path))
        {
            Ok(file) => self.output = Some(file),
            Err(error) => tracing::error!("Failed to finalize storage metrics: {error}"),
        }
        tracing::info!("Storage metrics tracking enabled - output: {name}");
    }

    /// Adds the line of a task whose folder still exists.
    fn measure(&mut self, task: &Task) {
        let Some(folder) = folder(task).filter(|folder| folder.is_dir()) else {
            return;
        };
        let size = size(&folder);
        let field = |key: &str| match task.record.get(key) {
            Some(serde_json::Value::String(text)) => text.clone(),
            Some(serde_json::Value::Null) | None => "N/A".to_owned(),
            Some(other) => other.to_string(),
        };
        let tag = match task.record.get("tag") {
            Some(serde_json::Value::String(tag)) => tag.clone(),
            _ => "-".to_owned(),
        };
        let line = format!(
            "{}\t{}\t{}\t{tag}\tCOMPLETED\t{}\t{size}\t{}\n",
            field("task_id"),
            field("hash"),
            field("process"),
            folder.display(),
            human(size)
        );
        let Some(output) = &mut self.output else {
            return;
        };
        let header = if self.header { "" } else { HEADER };
        if let Err(error) = output
            .write_all(format!("{header}{line}").as_bytes())
            .and_then(|()| output.flush())
        {
            tracing::warn!(
                "Failed to calculate storage metrics for task {}: {error}",
                field("hash")
            );
            return;
        }
        self.header = true;
        self.count += 1;
        self.total += size;
    }

    fn ended(&mut self) {
        self.output.take();
        tracing::info!("Storage metrics written to: {}", self.path.display());
        let average = self.total.checked_div(self.count).unwrap_or(0);
        tracing::info!(
            "\nStorage Metrics Summary:\n  Total tasks:     {}\n  Total size:      {}\n  \
             Average/task:    {}\n  Metrics file:    {}",
            self.count,
            human(self.total),
            human(average),
            self.file.as_deref().unwrap_or_default()
        );
    }
}

/// The folder of a task as its outputs show it; uwe keeps the rest of a task's folder to its
/// executor, so a task without output files has none.
fn folder(task: &Task) -> Option<PathBuf> {
    task.outputs.iter().find_map(|output| {
        let folder = output.path.strip_suffix(&output.name)?.strip_suffix('/')?;
        folder.starts_with('/').then(|| PathBuf::from(folder))
    })
}

/// The size of the regular files below a folder; a link to a file counts with its target, a
/// linked folder is not walked.
fn size(folder: &Path) -> u64 {
    let mut total = 0;
    for entry in std::fs::read_dir(folder).into_iter().flatten().flatten() {
        let path = entry.path();
        let linked = entry.file_type().is_ok_and(|kind| kind.is_symlink());
        match std::fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() => total += metadata.len(),
            Ok(metadata) if metadata.is_dir() && !linked => total += size(&path),
            Ok(_) => {}
            Err(error) => tracing::warn!("Error walking directory {}: {error}", folder.display()),
        }
    }
    total
}

/// A size in Nextflow's memory units, 1024 to a step, with at most one decimal, like `4.7 KB`.
fn human(bytes: u64) -> String {
    const UNITS: [&str; 7] = ["B", "KB", "MB", "GB", "TB", "PB", "EB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    let text = format!("{:.1}", (value * 10.0).round() / 10.0);
    let text = text.strip_suffix(".0").unwrap_or(&text);
    format!("{text} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prints_memory_units() {
        assert_eq!(human(1_504_500), "1.4 MB");
        assert_eq!(human(1_504_500 / 3), "489.7 KB");
        assert_eq!(human(4800), "4.7 KB");
        assert_eq!(human(0), "0 B");
    }
}
