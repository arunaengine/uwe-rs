//! nf-prov 1.8.0: provenance reports of a successful run, as a BioCompute Object, a Mermaid
//! task graph, a GEXF graph and a Workflow Run RO-Crate.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod actions;
mod bco;
mod dag;
mod data;
mod files;
mod gexf;
mod people;
mod wrroc;

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use nextflow_config::Config;
use nextflow_evaluator::value::Value;

use crate::catalogue::{Kind, Plugin, State};
use crate::observer::{End, Event, Observer, Start, Task};

pub(crate) const PLUGIN: Plugin = Plugin {
    scopes: &["prov"],
    observer: Some(activate),
    ..Plugin::new("nf-prov", Kind::Observer, State::Implemented)
};

/// A report format, in the order the run end writes them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Format {
    Bco,
    Dag,
    Gexf,
    Wrroc,
}

impl Format {
    const ALL: [Format; 4] = [Format::Bco, Format::Dag, Format::Gexf, Format::Wrroc];

    fn key(self) -> &'static str {
        match self {
            Format::Bco => "bco",
            Format::Dag => "dag",
            Format::Gexf => "gexf",
            Format::Wrroc => "wrroc",
        }
    }
}

/// A report that the config asks for, with the file it goes to.
struct Report {
    format: Format,
    file: PathBuf,
}

/// Reads the `prov` scope and checks the report files before any task runs: an existing
/// file stops the run, unless `overwrite` lets the plugin delete it.
fn activate(config: &Config) -> Result<Option<Box<dyn Observer>>, String> {
    if matches!(
        config.get(&["prov", "enabled"]),
        Some(Value::Boolean(false))
    ) {
        return Ok(None);
    }
    if config.get(&["prov", "formats", "legacy"]).is_some() {
        tracing::warn!("The `legacy` provenance format is no longer supported");
    }
    let launch = std::env::current_dir().map_err(|error| error.to_string())?;
    let output_dir = (config.get(&["outputDir"]).map(|value| value.to_string()))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "results".to_owned());
    let output_dir = launch.join(output_dir);
    let directory = (config
        .get(&["prov", "directory"])
        .map(|value| value.to_string()))
    .filter(|value| !value.is_empty());
    let base = match &directory {
        Some(directory) => output_dir.join(inside(directory)?),
        None => launch.clone(),
    };
    let mut reports = Vec::new();
    for format in Format::ALL {
        let scope = config.get(&["prov", "formats", format.key()]);
        let filled =
            |map: &Value| matches!(map, Value::Map(map) if !map.borrow().entries.is_empty());
        if !scope.as_ref().is_some_and(filled) {
            continue;
        }
        let Some(file) = config.get(&["prov", "formats", format.key(), "file"]) else {
            continue;
        };
        let file = file.to_string();
        if directory.is_some() && Path::new(&file).is_absolute() {
            return Err(format!(
                "Config option `prov.directory` cannot be used when a report file is an absolute \
                 path -- offending value: {file}"
            ));
        }
        let file = base.join(&file);
        let overwrite = matches!(
            config.get(&["prov", "formats", format.key(), "overwrite"]),
            Some(value) if value.truth()
        );
        clear(&file, overwrite)?;
        reports.push(Report { format, file });
    }
    if reports.is_empty() {
        tracing::warn!(
            "Config setting `prov.formats` is not defined -- no provenance reports will be produced"
        );
        return Ok(None);
    }
    let patterns = match config.get(&["prov", "patterns"]) {
        Some(Value::List(list)) => list.borrow().items.iter().map(Value::to_string).collect(),
        _ => Vec::new(),
    };
    Ok(Some(Box::new(Provenance {
        reports,
        patterns,
        output_dir,
        start: None,
        tasks: Vec::new(),
        published: Vec::new(),
    })))
}

/// `prov.directory`, which must stay inside the output directory.
fn inside(directory: &str) -> Result<PathBuf, String> {
    if Path::new(directory).is_absolute() {
        return Err(format!(
            "Config option `prov.directory` must be a relative path -- offending value: {directory}"
        ));
    }
    let mut normal = PathBuf::new();
    for component in Path::new(directory).components() {
        match component {
            Component::Normal(part) => normal.push(part),
            Component::ParentDir if normal.pop() => {}
            Component::CurDir => {}
            _ => {
                return Err(format!(
                    "Config option `prov.directory` cannot refer to a location outside of the \
                     workflow output directory -- offending value: {directory}"
                ));
            }
        }
    }
    Ok(normal)
}

/// Fails for an existing report file, or deletes it when the config lets it be replaced.
fn clear(file: &Path, overwrite: bool) -> Result<(), String> {
    if !file.exists() {
        return Ok(());
    }
    if !overwrite {
        return Err(format!(
            "Provenance file already exists: {}",
            file.display()
        ));
    }
    if file.is_dir() || std::fs::remove_file(file).is_err() {
        return Err(format!(
            "Unable to overwrite existing provenance file: {}",
            file.display()
        ));
    }
    Ok(())
}

/// What the reports record of a run: its start, the tasks that succeeded or were cached, and
/// the files it published.
struct Provenance {
    reports: Vec<Report>,
    patterns: Vec<String>,
    output_dir: PathBuf,
    start: Option<Box<Start>>,
    tasks: Vec<Task>,
    /// Each published target with its source, in the order of the first publish.
    published: Vec<(String, String)>,
}

impl Observer for Provenance {
    fn notify(&mut self, event: &Event) {
        match event {
            Event::RunStarted(start) => self.start = Some(start.clone()),
            Event::TaskCompleted(task) | Event::TaskCached(task) => self.tasks.push(task.clone()),
            Event::FilePublished { source, target } if self.wanted(target) => {
                match self.published.iter_mut().find(|(known, _)| known == target) {
                    Some(entry) => entry.1 = source.clone(),
                    None => self.published.push((target.clone(), source.clone())),
                }
            }
            Event::RunEnded(end) if end.success => self.write(end),
            _ => {}
        }
    }
}

impl Provenance {
    fn wanted(&self, target: &str) -> bool {
        let target = target.trim_start_matches('/');
        self.patterns.is_empty()
            || (self.patterns.iter()).any(|pattern| {
                nextflow_evaluator::interpreter::glob::matches(&format!("**/{pattern}"), target)
            })
    }

    /// Writes each report; one that fails is reported and the next is still written.
    fn write(&self, end: &End) {
        let Some(start) = &self.start else {
            return;
        };
        let mut outputs = Vec::new();
        for output in &end.outputs {
            let files: Vec<String> = match &output.index {
                Some(index) => vec![index.clone()],
                None => output.files.clone(),
            };
            if files.is_empty() {
                tracing::warn!(
                    "Workflow output `{}` does not contain any files and will not be included \
                     in provenance reports",
                    output.name
                );
                continue;
            }
            outputs.push((output.name.clone(), files));
        }
        let run = Run {
            start,
            end,
            tasks: &self.tasks,
            published: &self.published,
            outputs,
            output_dir: &self.output_dir,
            steps: steps(start),
        };
        for report in &self.reports {
            let pointer = format!("/prov/formats/{}", report.format.key());
            let options = start.config.pointer(&pointer).cloned();
            let options = options.unwrap_or_default();
            let written = match report.format {
                Format::Bco => save(&report.file, &bco::render(&run, &options)),
                Format::Dag => save(&report.file, &dag::render(&run)),
                Format::Gexf => save(&report.file, &gexf::render(&run)),
                Format::Wrroc => wrroc::write(&run, &options, &report.file),
            };
            if let Err(error) = written {
                let label = report.format.key().to_uppercase();
                tracing::warn!(
                    "Error occurred while rendering {label} provenance report -- see Nextflow log \
                     for details"
                );
                tracing::debug!(%error, "the {label} provenance report failed");
            }
        }
    }
}

/// A finished run as the reports see it.
struct Run<'a> {
    start: &'a Start,
    end: &'a End,
    tasks: &'a [Task],
    published: &'a [(String, String)],
    /// Each workflow output with files, by name: its index file, else its files.
    outputs: Vec<(String, Vec<String>)>,
    output_dir: &'a Path,
    /// The process step of each process, by its number.
    steps: HashMap<u32, u32>,
}

impl Run<'_> {
    /// The process step of a task, 0 when its process is not known.
    fn step(&self, task: &Task) -> u32 {
        (task.process_id)
            .and_then(|id| self.steps.get(&id).copied())
            .unwrap_or(0)
    }

    /// The input files of the tasks that no recorded task made, in the order tasks staged
    /// them.
    fn inputs(&self) -> Vec<String> {
        let made: Vec<&str> = (self.tasks.iter())
            .flat_map(|task| task.outputs.iter().map(|file| file.path.as_str()))
            .collect();
        let mut seen = HashSet::new();
        let mut inputs = Vec::new();
        for task in self.tasks {
            for input in &task.inputs {
                let path = input.path.as_str();
                let produced = (made.iter())
                    .any(|output| path == *output || path.starts_with(&format!("{output}/")));
                if !produced && seen.insert(path) {
                    inputs.push(path.to_owned());
                }
            }
        }
        inputs
    }

    /// The task that made a file, with the file's name in its work directory.
    fn producer(&self, path: &str) -> Option<(usize, &str)> {
        (self.tasks.iter().enumerate()).find_map(|(index, task)| {
            (task.outputs.iter())
                .find(|output| output.path == path)
                .map(|output| (index, output.name.as_str()))
        })
    }
}

/// For each process, the number of processes on the longest path to it in the process graph,
/// itself included; a path that closes a cycle counts 0 there.
fn steps(start: &Start) -> HashMap<u32, u32> {
    let mut feeders: HashMap<u32, Vec<u32>> = HashMap::new();
    for process in &start.processes {
        for fed in &process.feeds {
            feeders.entry(*fed).or_default().push(process.id);
        }
    }
    let mut steps = HashMap::new();
    for process in &start.processes {
        depth(process.id, &feeders, &mut steps, &mut Vec::new());
    }
    steps
}

fn depth(
    id: u32,
    feeders: &HashMap<u32, Vec<u32>>,
    steps: &mut HashMap<u32, u32>,
    path: &mut Vec<u32>,
) -> u32 {
    if let Some(step) = steps.get(&id) {
        return *step;
    }
    if path.contains(&id) {
        return 0;
    }
    path.push(id);
    let before = (feeders.get(&id).into_iter().flatten())
        .map(|feeder| depth(*feeder, feeders, steps, path))
        .max()
        .unwrap_or(0);
    path.pop();
    steps.insert(id, before + 1);
    before + 1
}

/// A path as Nextflow's path normaliser writes it: a local path as a `file://` address, an
/// address as it is.
fn normalise(path: &str) -> String {
    if path.starts_with('/') {
        format!("file://{path}")
    } else {
        path.to_owned()
    }
}

/// The last part of a path or address.
fn file_name(path: &str) -> &str {
    let path = path.trim_end_matches('/');
    path.rsplit('/').next().unwrap_or(path)
}

/// The media type of a file by its name, `None` for a directory, a missing local file or a
/// name without a known type.
fn media_type(path: &str) -> Option<&'static str> {
    if path.starts_with('/') && !Path::new(path).is_file() {
        return None;
    }
    let name = file_name(path).to_ascii_lowercase();
    let extension = name.rsplit_once('.')?.1;
    Some(match extension {
        "txt" | "log" | "text" => "text/plain",
        "json" => "application/json",
        "html" | "htm" => "text/html",
        "csv" => "text/csv",
        "tsv" => "text/tab-separated-values",
        "xml" => "application/xml",
        "md" => "text/markdown",
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "gz" => "application/gzip",
        "zip" => "application/zip",
        "tar" => "application/x-tar",
        "yml" | "yaml" => "application/yaml",
        "js" => "application/javascript",
        "sh" => "application/x-sh",
        _ => return None,
    })
}

/// The first 32 hex digits of a task's hash, the length of a Nextflow task hash.
fn task_hash(task: &Task) -> &str {
    task.hash.get(..32).unwrap_or(&task.hash)
}

/// Writes a report file, with the folders it needs.
fn save(file: &Path, text: &str) -> std::io::Result<()> {
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(file, text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observer::Process;

    #[test]
    fn numbers_steps() {
        let process = |id: u32, feeds: &[u32]| Process {
            id,
            name: format!("P{id}"),
            definition: format!("P{id}"),
            script: None,
            feeds: feeds.to_vec(),
        };
        let processes = vec![process(1, &[2, 3]), process(2, &[3]), process(3, &[2])];
        let start = Start {
            processes,
            ..crate::nf_prov::tests::start()
        };
        let steps = steps(&start);
        assert_eq!((steps[&1], steps[&2], steps[&3]), (1, 3, 2));
    }

    pub(crate) fn start() -> Start {
        Start {
            run_name: String::new(),
            session_id: String::new(),
            launch_dir: String::new(),
            work_dir: String::new(),
            project_dir: String::new(),
            script: String::new(),
            project_name: String::new(),
            repository: None,
            commit_id: None,
            revision: None,
            command_line: String::new(),
            profile: String::new(),
            config_files: Vec::new(),
            params: serde_json::Value::Null,
            config: serde_json::Value::Null,
            manifest: serde_json::Value::Null,
            processes: Vec::new(),
            start: 0,
            resume: false,
        }
    }

    #[test]
    fn keeps_directory_inside() {
        assert!(inside("../x").is_err());
        assert!(inside("/x").is_err());
        assert_eq!(inside("a/../b"), Ok(PathBuf::from("b")));
    }
}
