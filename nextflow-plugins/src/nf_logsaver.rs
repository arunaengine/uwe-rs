//! nf-logsaver 0.1.0: keeps the standard output, the standard error and the script of each
//! finished task in one folder of the output directory.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use nextflow_config::Config;

use crate::catalogue::{Kind, Plugin, State};
use crate::observer::{Event, Observer, Start, Task};

pub(crate) const PLUGIN: Plugin = Plugin {
    functions: &["sayHello"],
    call: Some(crate::nf_storagemetrics::say_hello),
    observer: Some(activate),
    ..Plugin::new("nf-logsaver", Kind::Observer, State::Implemented)
};

/// The folder below the output directory that holds the copies.
const FOLDER: &str = "logsaver_command_files";

fn activate(_: &Config) -> Result<Option<Box<dyn Observer>>, String> {
    Ok(Some(Box::new(LogSaver {
        folder: PathBuf::new(),
    })))
}

struct LogSaver {
    folder: PathBuf,
}

impl Observer for LogSaver {
    fn notify(&mut self, event: &Event) {
        match event {
            Event::RunStarted(start) => self.started(start),
            Event::TaskCompleted(task) | Event::TaskFailed(task) => self.save(task),
            _ => {}
        }
    }
}

impl LogSaver {
    /// The folder is `<params.outdir or results>/logsaver_command_files` of the launch
    /// directory; the plugin says when it makes it.
    fn started(&mut self, start: &Start) {
        let outdir = (start.params["outdir"].as_str())
            .filter(|outdir| !outdir.is_empty())
            .unwrap_or("results");
        let built = format!("{outdir}/{FOLDER}");
        self.folder = Path::new(&start.launch_dir).join(&built);
        if !self.folder.exists() {
            match std::fs::create_dir_all(&self.folder) {
                Ok(()) => tracing::info!("Created plugin log directory: {built}"),
                Err(error) => tracing::warn!(%error, "{built} was not made"),
            }
        }
    }

    /// Writes `<base>.err`, `<base>.out` and `<base>.sh`, where the base is the task name
    /// without its workflows, in lower case, and the first six digits of its hash.
    fn save(&self, task: &Task) {
        let name = task
            .record
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let name = name.rsplit(':').next().unwrap_or(name).to_lowercase();
        let hash: String = task.hash.chars().take(6).collect();
        let base = format!("{name}_{hash}");
        let script = task
            .record
            .get("script")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let files = [
            ("err", task.stderr.clone()),
            ("out", task.stdout.clone()),
            ("sh", command(script)),
        ];
        for (extension, text) in files {
            let path = self.folder.join(format!("{base}.{extension}"));
            if let Err(error) = std::fs::write(&path, text) {
                tracing::warn!(%error, "{} was not written", path.display());
            }
        }
    }
}

/// The text of `.command.sh`: the script with Nextflow's first line, when it has none yet.
fn command(script: &str) -> String {
    let body = dedent(script);
    match body.starts_with("#!") {
        true => format!("{body}\n"),
        false => format!("#!/bin/bash -ue\n{body}\n"),
    }
}

/// A script without the indent that all its lines share and without blank lines around it,
/// as Nextflow writes `.command.sh`.
fn dedent(script: &str) -> String {
    let indent = (script.lines())
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.len() - line.trim_start().len())
        .min()
        .unwrap_or(0);
    let lines: Vec<&str> = (script.lines())
        .map(|line| line.get(indent..).unwrap_or(line.trim_start()))
        .collect();
    lines.join("\n").trim_matches('\n').trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedents_scripts() {
        let script = "\n    echo \"out A\"\n    echo \"err A\" >&2\n    ";
        assert_eq!(dedent(script), "echo \"out A\"\necho \"err A\" >&2");
    }
}
