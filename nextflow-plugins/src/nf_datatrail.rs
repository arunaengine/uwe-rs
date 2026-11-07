//! nf-datatrail 0.0.1: records the files that each task read and wrote, and writes them at the
//! end of the run as CSV lists, a per-task summary, and a graph of tasks in DOT and JSON.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use nextflow_config::Config;
use nextflow_evaluator::regex::Pattern;
use nextflow_evaluator::units::memory_text;
use nextflow_evaluator::value::Value;

use crate::catalogue::{Kind, Plugin, State};
use crate::observer::{Event, Observer, Task};
use crate::report_format::Json;

pub(crate) const PLUGIN: Plugin = Plugin {
    scopes: &["datatrail"],
    observer: Some(activate),
    ..Plugin::new("nf-datatrail", Kind::Observer, State::Implemented)
};

/// What the `datatrail` scope asks for.
struct Settings {
    plot: Option<Plot>,
    persist: Option<String>,
    input: Option<String>,
    output: Option<String>,
    summary: Option<String>,
    delimiter: String,
}

/// How the graph is drawn.
struct Plot {
    file: String,
    rankdir: String,
    detailed: bool,
    external: bool,
    legend: bool,
    cluster: bool,
    tag_names: bool,
    filter: Vec<Pattern>,
}

fn activate(
    config: &Config,
    launch: &std::path::Path,
    _: &crate::catalogue::Environment,
) -> Result<Option<Box<dyn Observer>>, String> {
    let launch = launch.to_path_buf();
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
        let plot = match text(&["plot", "file"]) {
            None => None,
            Some(file) => {
                let filter = match get(&["plot", "filter"]) {
                    Some(Value::List(list)) => {
                        list.borrow().items.iter().map(Value::to_string).collect()
                    }
                    Some(other) => vec![other.to_string()],
                    None => Vec::new(),
                };
                let filter = (filter.iter())
                    .map(|text| Pattern::new(text).map_err(|error| error.to_string()))
                    .collect::<Result<Vec<_>, _>>()?;
                let written = dot_file(Path::new(&file));
                prepare(
                    &launch.join(&written),
                    &written.to_string_lossy(),
                    "DAG",
                    overwrite,
                )?;
                Some(Plot {
                    rankdir: text(&["plot", "rankdir"]).unwrap_or_else(|| "TB".to_owned()),
                    detailed: flag(&["plot", "detailed"], false),
                    external: flag(&["plot", "external"], true),
                    legend: flag(&["plot", "legend"], true),
                    cluster: flag(&["plot", "cluster"], false),
                    tag_names: flag(&["plot", "tagNames"], true),
                    file,
                    filter,
                })
            }
        };
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
        if plot.is_none() && lists.iter().all(Option::is_none) {
            return Ok(None);
        }
        let summary = lists.pop().flatten();
        let output = lists.pop().flatten();
        let input = lists.pop().flatten();
        Ok(Some(Settings {
            persist: text(&["persist"]).filter(|_| plot.is_some()),
            plot,
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

/// The DOT file that uwe writes for a plot file, which is a `.dot` beside other formats.
fn dot_file(target: &Path) -> PathBuf {
    match target.extension().and_then(|extension| extension.to_str()) {
        Some("dot") => target.to_path_buf(),
        _ => target.with_extension("dot"),
    }
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
    process: String,
    tag: Option<String>,
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
            process: text("process").unwrap_or_default(),
            tag: text("tag"),
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

    /// Writes the graph and the summary, and closes the lists.
    fn finish(&mut self) {
        self.inputs = None;
        self.outputs = None;
        if let Some(plot) = &self.settings.plot {
            println!("Rendering datatrail graph to file: {}", plot.file);
            let graph = Graph::of(&self.tasks, plot);
            let target = self.launch.join(&plot.file);
            let dot = dot_file(&target);
            if dot != target {
                tracing::warn!(
                    "Graphviz is required to render the execution DAG in the given format -- See \
                     http://www.graphviz.org for more info."
                );
            }
            let name = (target
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned()))
            .unwrap_or_default();
            write(&dot, &graph.dot(&self.tasks, plot, &name));
            if let Some(persist) = &self.settings.persist {
                write(
                    &self.launch.join(persist),
                    &graph.json(&self.tasks).pretty(),
                );
            }
        }
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

/// The node graph of the tasks that the filter shows.
struct Graph {
    /// The node id of each shown task, by place.
    nodes: Vec<Option<String>>,
    /// The external point of each shown task that read files no task made.
    origins: Vec<Option<String>>,
    ranks: Vec<usize>,
    /// The colour of each process name, by sorted name.
    colours: Vec<(String, String)>,
    /// The common `:` prefix of all process names, which labels leave out.
    prefix: String,
    edges: Vec<Edge>,
}

struct Edge {
    from: Option<usize>,
    to: usize,
    files: u64,
    bytes: u64,
}

impl Graph {
    fn of(tasks: &[Record], plot: &Plot) -> Graph {
        let hidden = |task: &Record| {
            (plot.filter.iter()).any(|pattern| pattern.matches(&task.process).unwrap_or(false))
        };
        let nodes: Vec<Option<String>> = (tasks.iter().enumerate())
            .map(|(place, task)| (!hidden(task)).then(|| format!("p{place}")))
            .collect();
        let mut count = 0;
        let origins = (tasks.iter().zip(&nodes))
            .map(|(task, node)| {
                let has = plot.external && node.is_some() && task.external.0 > 0;
                has.then(|| {
                    count += 1;
                    format!("o{}", count - 1)
                })
            })
            .collect();
        let mut ranks: Vec<usize> = Vec::with_capacity(tasks.len());
        for task in tasks {
            let rank = task
                .producers
                .iter()
                .map(|producer| ranks[producer.0] + 1)
                .max();
            ranks.push(rank.unwrap_or(1));
        }
        let names: BTreeSet<&str> = (tasks.iter().zip(&nodes))
            .filter(|(_, node)| node.is_some())
            .map(|(task, _)| task.process.as_str())
            .collect();
        let colours = (names.iter().enumerate())
            .map(|(index, name)| (name.to_string(), colour(index as f32 / names.len() as f32)))
            .collect();
        let mut edges = Vec::new();
        for (place, task) in tasks
            .iter()
            .enumerate()
            .filter(|(place, _)| nodes[*place].is_some())
        {
            if plot.external && task.external.0 > 0 {
                let (files, bytes) = task.external;
                edges.push(Edge {
                    from: None,
                    to: place,
                    files,
                    bytes,
                });
            }
            for &(producer, files, bytes) in &task.producers {
                if nodes[producer].is_some() {
                    edges.push(Edge {
                        from: Some(producer),
                        to: place,
                        files,
                        bytes,
                    });
                }
            }
        }
        Graph {
            prefix: prefix(names.iter().copied()),
            nodes,
            origins,
            ranks,
            colours,
            edges,
        }
    }

    fn colour(&self, process: &str) -> &str {
        (self.colours.iter())
            .find(|(name, _)| name == process)
            .map_or("#000000", |(_, colour)| colour)
    }

    fn label<'a>(&self, name: &'a str) -> &'a str {
        name.strip_prefix(self.prefix.as_str())
            .filter(|_| !self.prefix.is_empty())
            .unwrap_or(name)
    }

    /// The graph in DOT, as the plugin writes it.
    fn dot(&self, tasks: &[Record], plot: &Plot, name: &str) -> String {
        let name: String = name
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        let mut text = format!("digraph \"{name}\" {{\n  rankdir={};\n", plot.rankdir);
        text.push_str(if plot.detailed {
            "  ranksep=1.0;\n"
        } else {
            "  ranksep=0.4;\n"
        });
        if self.origins.iter().any(Option::is_some) {
            text.push_str("  // Inputs\n  subgraph inputs {\n    rank = same;\n");
            for origin in self.origins.iter().flatten() {
                text.push_str(&format!(
                    "    {origin} [shape=point,label=\"\",fixedsize=true,width=0.1,tooltip=\"External data input\"];\n"
                ));
            }
            text.push_str("  }\n");
        }
        text.push_str("  // Vertices\n");
        let shown: Vec<usize> = (0..tasks.len())
            .filter(|place| self.nodes[*place].is_some())
            .collect();
        if plot.cluster {
            let mut groups: Vec<Option<&str>> = Vec::new();
            for place in &shown {
                let tag = tasks[*place].tag.as_deref();
                if !groups.contains(&tag) {
                    groups.push(tag);
                }
            }
            for group in groups {
                let members = shown
                    .iter()
                    .filter(|place| tasks[**place].tag.as_deref() == group);
                let Some(tag) = group else {
                    members.for_each(|place| text.push_str(&self.node(tasks, *place, plot, "  ")));
                    continue;
                };
                let clean: String = tag.chars().filter(char::is_ascii_alphanumeric).collect();
                let title = if plot.tag_names {
                    format!("cluster_{clean}")
                } else {
                    clean
                };
                text.push_str(&format!(
                    "  subgraph \"{title}\" {{\n    label=\"{}\";\n    style=\"rounded\";\n",
                    quoted(tag)
                ));
                members.for_each(|place| text.push_str(&self.node(tasks, *place, plot, "    ")));
                text.push_str("  }\n");
            }
        } else {
            let top = shown
                .iter()
                .map(|place| self.ranks[*place])
                .max()
                .unwrap_or(0);
            for rank in 1..=top {
                let members: Vec<&usize> = shown
                    .iter()
                    .filter(|place| self.ranks[**place] == rank)
                    .collect();
                if members.is_empty() {
                    continue;
                }
                text.push_str(&format!(
                    "  subgraph \"Rank: {rank}\" {{\n    rank = same;\n"
                ));
                members
                    .iter()
                    .for_each(|place| text.push_str(&self.node(tasks, **place, plot, "    ")));
                text.push_str("  }\n");
            }
        }
        text.push_str("  // Edges\n");
        let largest = self.edges.iter().map(|edge| edge.bytes).max().unwrap_or(0);
        for edge in &self.edges {
            let from = match edge.from {
                Some(producer) => self.nodes[producer].clone().unwrap_or_default(),
                None => self.origins[edge.to].clone().unwrap_or_default(),
            };
            let to = self.nodes[edge.to].clone().unwrap_or_default();
            let amount = format!("{} {}", files(edge.files), size(edge.bytes));
            let mut line = match plot.detailed {
                true => format!("  {from} -> {to} [label=\"{amount}\",fontsize=10"),
                false => format!("  {from} -> {to} [label=\"\",tooltip=\"{amount}\""),
            };
            if edge.bytes > 0 {
                line.push_str(&format!(",penwidth={}", pen_width(edge.bytes, largest)));
            }
            let colour = self.colour(&tasks[edge.to].process);
            text.push_str(&format!("{line},color=\"{colour}\"];\n"));
        }
        if plot.legend {
            text.push_str(
                "  // Legend\n  legend [shape=plain, margin=0.2, label=<\n    <table border=\"0\" \
                 cellborder=\"1\" cellspacing=\"0\">\n      <tr><td colspan=\"2\"><b>Tasks</b></td></tr>\n",
            );
            for (process, colour) in &self.colours {
                let process = html(self.label(process));
                text.push_str(&format!(
                    "    <tr><td bgcolor=\"{colour}\"></td><td>{process}</td></tr>\n"
                ));
            }
            text.push_str("    </table>\n  >];\n  { rank=sink; legend; }\n");
        }
        text.push_str("}\n");
        text
    }

    /// A task node: a table with its volumes when detailed, else a point with a tooltip.
    fn node(&self, tasks: &[Record], place: usize, plot: &Plot, indent: &str) -> String {
        let task = &tasks[place];
        let id = self.nodes[place].clone().unwrap_or_default();
        let colour = self.colour(&task.process);
        let total = |entries: &[Entry]| entries.iter().map(|entry| entry.size).sum::<u64>();
        let (inputs, outputs) = (task.inputs.len() as u64, task.outputs.len() as u64);
        let (read, written) = (total(&task.inputs), total(&task.outputs));
        let name = self.label(&task.name);
        if !plot.detailed {
            return format!(
                "{indent}{id} [label=\"\",shape=point,width=0.2,tooltip=\"{}\n{indent}Inputs: {} {}\n\
                 {indent}Outputs: {} {}\",style=filled,fillcolor=\"{colour}\"];\n\n",
                quoted(name),
                files(inputs),
                size(read),
                files(outputs),
                size(written)
            );
        }
        let mut text = format!(
            "  {id} [label=< \n        <table border=\"0\" cellspacing=\"0\">\n          \
             <tr><td colspan=\"4\"><b>{}</b></td></tr>\n",
            html(name)
        );
        for (title, count, bytes) in [("Input:", inputs, read), ("Output:", outputs, written)] {
            if count > 0 {
                text.push_str(&format!(
                    "          <tr><td align=\"left\">{title}</td><td>{}</td><td>{}</td></tr>\n",
                    files(count),
                    size(bytes)
                ));
            }
        }
        text.push_str(&format!(
            "        </table>\n      >\n  ,style=filled,fillcolor=\"{colour}\"];\n\n"
        ));
        text
    }

    /// The graph as JSON, with every node in full at each end of its edges.
    fn json(&self, tasks: &[Record]) -> Json {
        let origin = |id: &str| {
            Json::object([
                ("ID", Json::from(id)),
                ("origin", Json::from(true)),
                ("process", Json::from(false)),
                ("rank", Json::from(0i64)),
            ])
        };
        let process = |place: usize| {
            let task = &tasks[place];
            let total = |entries: &[Entry]| entries.iter().map(|entry| entry.size).sum::<u64>();
            let read = total(&task.inputs);
            Json::object([
                ("outputFiles", Json::from(task.outputs.len() as u64)),
                ("outputSize", Json::from(total(&task.outputs))),
                ("inputFiles", Json::from(task.inputs.len() as u64)),
                ("inputSize", Json::from(read)),
                ("taskName", Json::from(&task.name)),
                ("processName", Json::from(&task.process)),
                ("rank", Json::from(self.ranks[place] as u64)),
                ("ID", Json::from(self.nodes[place].clone())),
                (
                    "label",
                    Json::from(format!(
                        "{} input: {} files ({read} bytes)",
                        task.name,
                        task.inputs.len()
                    )),
                ),
                ("origin", Json::from(false)),
                ("process", Json::from(true)),
                ("tag", Json::from(task.tag.clone())),
            ])
        };
        let origins = self.origins.iter().flatten().map(|id| origin(id)).collect();
        let shown = (0..tasks.len()).filter(|place| self.nodes[*place].is_some());
        let processes = shown.map(process).collect();
        let edges = (self.edges.iter())
            .map(|edge| {
                let from = match edge.from {
                    Some(producer) => process(producer),
                    None => origin(self.origins[edge.to].as_deref().unwrap_or_default()),
                };
                Json::object([
                    ("from", from),
                    ("to", process(edge.to)),
                    ("inputFiles", Json::from(edge.files)),
                    ("inputSize", Json::from(edge.bytes)),
                    ("ID", Json::Null),
                    (
                        "label",
                        Json::from(format!(
                            "Input: {} files ({} bytes)",
                            edge.files, edge.bytes
                        )),
                    ),
                ])
            })
            .collect();
        Json::object([
            ("origins", Json::List(origins)),
            ("processes", Json::List(processes)),
            ("edges", Json::List(edges)),
        ])
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

/// The longest run of leading `:` parts that every name shares, leaving each at least one.
fn prefix<'a>(names: impl Iterator<Item = &'a str>) -> String {
    let parts: Vec<Vec<&str>> = names.map(|name| name.split(':').collect()).collect();
    if parts.is_empty() || parts.iter().any(|name| name.len() < 2) {
        return String::new();
    }
    let most = parts.iter().map(|name| name.len() - 1).min().unwrap_or(0);
    let shared = (0..most)
        .take_while(|index| parts.iter().all(|name| name[*index] == parts[0][*index]))
        .count();
    match shared {
        0 => String::new(),
        _ => format!("{}:", parts[0][..shared].join(":")),
    }
}

/// A colour spread on the hue circle, as Java's `Color.HSBtoRGB` converts it.
fn colour(hue: f32) -> String {
    let (saturation, brightness) = (0.7f32, 0.9f32);
    let h = (hue - hue.floor()) * 6.0;
    let f = h - h.floor();
    let p = brightness * (1.0 - saturation);
    let q = brightness * (1.0 - saturation * f);
    let t = brightness * (1.0 - saturation * (1.0 - f));
    let (r, g, b) = match h as u32 {
        0 => (brightness, t, p),
        1 => (q, brightness, p),
        2 => (p, brightness, t),
        3 => (p, q, brightness),
        4 => (t, p, brightness),
        _ => (brightness, p, q),
    };
    let channel = |value: f32| (value * 255.0 + 0.5) as u8;
    format!("#{:02x}{:02x}{:02x}", channel(r), channel(g), channel(b))
}

/// `8 * (bytes / largest + 0.08)` in Groovy's decimals: an exact share, else one rounded half
/// up to ten decimals, whose scale the text keeps.
fn pen_width(bytes: u64, largest: u64) -> String {
    let (bytes, largest) = (u128::from(bytes), u128::from(largest.max(1)));
    let exact = (0..=10).find(|scale| (bytes * 10u128.pow(*scale)) % largest == 0);
    let (share, scale) = match exact {
        Some(own) => (
            bytes * 10u128.pow(own) / largest * 10u128.pow(own.max(2) - own),
            own.max(2),
        ),
        None => ((bytes * 10u128.pow(11) / largest + 5) / 10, 10),
    };
    let units = 8 * (share + 8 * 10u128.pow(scale - 2));
    let whole = units / 10u128.pow(scale);
    let fraction = units % 10u128.pow(scale);
    format!("{whole}.{fraction:0width$}", width = scale as usize)
}

fn files(count: u64) -> String {
    match count {
        1 => "1 file".to_owned(),
        _ => format!("{count} files"),
    }
}

/// A size as Nextflow's memory text without spaces, like `1.5KB`.
fn size(bytes: u64) -> String {
    memory_text(i64::try_from(bytes).unwrap_or(i64::MAX)).replace(' ', "")
}

fn quoted(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

fn html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_java_numbers() {
        assert_eq!(colour(0.0), "#e64545");
        assert_eq!(colour(1.0 / 3.0), "#45e645");
        assert_eq!(colour(2.0 / 3.0), "#4545e6");
        assert_eq!(colour(0.5), "#45e6e6");
        assert_eq!(pen_width(10, 36), "2.8622222224");
        assert_eq!(pen_width(10, 18), "5.0844444448");
        assert_eq!(
            (pen_width(18, 36), pen_width(36, 36)),
            ("4.64".into(), "8.64".into())
        );
        assert_eq!(
            (size(10), size(1536)),
            ("10B".to_owned(), "1.5KB".to_owned())
        );
        assert_eq!(prefix(["A:B:X", "A:B:Y"].into_iter()), "A:B:");
        assert_eq!(prefix(["A:X", "B"].into_iter()), "");
        assert_eq!(
            ancestors("/a/b/c").collect::<Vec<_>>(),
            ["/a/b/c", "/a/b", "/a"]
        );
    }
}
