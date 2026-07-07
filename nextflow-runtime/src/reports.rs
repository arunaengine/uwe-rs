//! Nextflow's own reports of a real run, as the `trace`, `timeline`, `report` and `dag` scopes
//! of the config turn them on: the trace of every task attempt, a timeline, a report with a
//! summary and the tasks, and the graph of the processes. nf-core pipelines turn all four on,
//! with files in `pipeline_info/`. The driver writes them at the end of the run into its own
//! folder and asks the engine to deliver them, so that a simulated run writes none.
//!
//! The trace has the columns that Nextflow's has. What iunctus does not measure yet, like the
//! CPU and memory that a task used, or has no counterpart for, like the hash of a work folder,
//! is `-`, which Nextflow writes for unknown values too.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use nextflow_config::Config;
use nextflow_evaluator::date;
use nextflow_evaluator::value::Value;
use workflow_graph::render::{Format, View, render};
use workflow_graph::{
    Delivery, DeliveryMode, Edge, EdgeKind, ExecutionGraph, Language, Node, Outcome, Task, TaskId,
};

/// The columns of the trace when the config names none, as in Nextflow.
const FIELDS: [&str; 14] = [
    "task_id",
    "hash",
    "native_id",
    "name",
    "status",
    "exit",
    "submit",
    "duration",
    "realtime",
    "%cpu",
    "peak_rss",
    "peak_vmem",
    "rchar",
    "wchar",
];

/// A value that is not known.
const UNKNOWN: &str = "-";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Trace,
    Timeline,
    Report,
    Dag,
}

/// The reports that the config asks for, and what the run told them so far.
pub(crate) struct Reports {
    /// Each report that the config turns on, with the file it goes to.
    wanted: Vec<(Kind, PathBuf)>,
    fields: Vec<String>,
    separator: String,
    workflow: String,
    /// When the run started, in milliseconds since 1970.
    started: i64,
    /// Each attempt that ended, in the order they ended.
    rows: Vec<Row>,
    /// Each task asked for, with when it was last asked for.
    asked: HashMap<TaskId, (Task, i64)>,
    /// The tasks in the order they were first asked for; the number of a task is its place.
    order: Vec<TaskId>,
    /// The exit code of the last attempt of each task.
    exits: HashMap<TaskId, i32>,
    /// The task that made each output, by its location.
    producers: HashMap<String, TaskId>,
}

/// One attempt of a task that ended.
struct Row {
    number: usize,
    task: Task,
    exit: i32,
    attempt: u32,
    cached: bool,
    image: Option<String>,
    submit: i64,
    start: Option<i64>,
    complete: Option<i64>,
}

impl Reports {
    /// The reports that the config turns on; files with a relative path are in `launch_dir`.
    pub fn of(config: &Config, launch_dir: &Path, workflow: &str) -> Reports {
        let started = now();
        let stamp = date::format(started, "yyyyMMdd-").unwrap_or_default();
        let stamp = format!("{stamp}{}", millis_of_day(started));
        let mut wanted = Vec::new();
        for (kind, scope, default) in [
            (Kind::Trace, "trace", format!("trace-{stamp}.txt")),
            (Kind::Timeline, "timeline", format!("timeline-{stamp}.html")),
            (Kind::Report, "report", format!("report-{stamp}.html")),
            (Kind::Dag, "dag", format!("dag-{stamp}.html")),
        ] {
            if !config
                .get(&[scope, "enabled"])
                .is_some_and(|value| value.truth())
            {
                continue;
            }
            let file = (config.get(&[scope, "file"]).map(|file| file.to_string()))
                .filter(|file| !file.is_empty())
                .unwrap_or(default);
            wanted.push((kind, launch_dir.join(file)));
        }
        let fields = match config.get(&["trace", "fields"]) {
            Some(Value::List(list)) => (list.borrow().items.iter())
                .map(|field| field.to_string())
                .collect(),
            Some(value) => (value.to_string().split(','))
                .map(|field| field.trim().to_owned())
                .filter(|field| !field.is_empty())
                .collect(),
            None => FIELDS.map(str::to_owned).to_vec(),
        };
        let separator = (config.get(&["trace", "sep"]).map(|sep| sep.to_string()))
            .filter(|sep| !sep.is_empty())
            .unwrap_or_else(|| "\t".to_owned());
        Reports {
            wanted,
            fields,
            separator,
            workflow: workflow.to_owned(),
            started,
            rows: Vec::new(),
            asked: HashMap::new(),
            order: Vec::new(),
            exits: HashMap::new(),
            producers: HashMap::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.wanted.is_empty()
    }

    /// Notes that the run asked for a task, or for another attempt of it.
    pub fn asked(&mut self, task: &Task) {
        if !self.asked.contains_key(&task.id) {
            self.order.push(task.id);
        }
        self.asked.insert(task.id, (task.clone(), now()));
    }

    /// Notes how an attempt of a task ended.
    pub fn ended(&mut self, outcome: &Outcome) {
        let Some((task, submit)) = self.asked.get(&outcome.id) else {
            return;
        };
        let number = self.order.iter().position(|id| *id == outcome.id);
        self.rows.push(Row {
            number: number.map_or(0, |place| place + 1),
            task: task.clone(),
            exit: outcome.exit_code,
            attempt: outcome.attempt,
            cached: outcome.cached,
            image: outcome.image.clone(),
            submit: *submit,
            start: outcome.started.map(millis),
            complete: outcome.ended.map(millis),
        });
        self.exits.insert(outcome.id, outcome.exit_code);
        for file in &outcome.files {
            self.producers.insert(file.location.clone(), outcome.id);
        }
    }

    /// Writes the reports into `folder`, and gives their deliveries to their files.
    pub fn write(&self, folder: &Path) -> std::io::Result<Vec<Delivery>> {
        std::fs::create_dir_all(folder)?;
        let mut deliveries = Vec::new();
        for (kind, target) in &self.wanted {
            let text = match kind {
                Kind::Trace => self.trace(),
                Kind::Timeline => self.timeline(),
                Kind::Report => self.report(),
                Kind::Dag => self.dag(),
            };
            let name = target
                .file_name()
                .map_or("report".into(), |name| name.to_owned());
            let written = folder.join(name);
            std::fs::write(&written, text)?;
            deliveries.push(Delivery {
                location: written.to_string_lossy().into_owned(),
                target: target.clone(),
                mode: DeliveryMode::Copy,
            });
        }
        Ok(deliveries)
    }

    /// The trace: a header with the fields, then one line per attempt.
    fn trace(&self) -> String {
        let mut text = self.fields.join(&self.separator);
        text.push('\n');
        for row in &self.rows {
            let values: Vec<String> = (self.fields.iter())
                .map(|field| row.value(field.split(':').next().unwrap_or_default()))
                .collect();
            text.push_str(&values.join(&self.separator));
            text.push('\n');
        }
        text
    }

    /// The timeline: a bar per attempt, from its start to its end.
    fn timeline(&self) -> String {
        let end = (self.rows.iter())
            .filter_map(|row| row.complete)
            .max()
            .unwrap_or(self.started);
        let span = (end - self.started).max(1) as f64;
        let (width, height) = (1000.0, 22 * self.rows.len() + 10);
        let mut bars = String::new();
        let mut rows: Vec<&Row> = self.rows.iter().collect();
        rows.sort_by_key(|row| row.start.unwrap_or(row.submit));
        for (index, row) in rows.iter().enumerate() {
            let x = |time: i64| 300.0 + (time - self.started) as f64 / span * (width - 320.0);
            let (from, to) = (
                x(row.start.unwrap_or(row.submit)),
                x(row.complete.unwrap_or(end)),
            );
            let y = 22 * index + 5;
            let color = if row.exit == 0 { "#4c9f70" } else { "#c0392b" };
            let _ = writeln!(
                bars,
                "<text x=\"0\" y=\"{}\">{}</text><rect x=\"{from:.1}\" y=\"{y}\" \
                 width=\"{:.1}\" height=\"16\" fill=\"{color}\"><title>{}</title></rect>",
                y + 13,
                escape(&row.task.name),
                (to - from).max(1.0),
                escape(&format!("{} ({})", row.status(), duration(row.realtime()))),
            );
        }
        let body = format!(
            "<p>Started {}, {} task attempts over {}.</p>\n<svg xmlns=\"http://www.w3.org/2000/svg\" \
             width=\"{width}\" height=\"{height}\" font-family=\"sans-serif\" \
             font-size=\"12\">\n{bars}</svg>\n",
            escape(&stamp(self.started)),
            self.rows.len(),
            duration(Some(end - self.started)),
        );
        page("Timeline", &self.workflow, &body)
    }

    /// The report: a summary of the run, then a table of the attempts.
    fn report(&self) -> String {
        let count = |status: &str| {
            self.rows
                .iter()
                .filter(|row| row.status() == status)
                .count()
        };
        let end = self.rows.iter().filter_map(|row| row.complete).max();
        let mut body = format!(
            "<p>Started {}, ran for {}: {} completed, {} cached, {} failed.</p>\n<table>\n<tr>",
            escape(&stamp(self.started)),
            duration(end.map(|end| end - self.started)),
            count("COMPLETED"),
            count("CACHED"),
            count("FAILED"),
        );
        let fields = ["task_id", "name", "status", "exit", "attempt", "container"];
        let fields = fields
            .iter()
            .chain(&["submit", "start", "duration", "realtime"]);
        for field in fields.clone() {
            let _ = write!(body, "<th>{field}</th>");
        }
        body.push_str("</tr>\n");
        for row in &self.rows {
            body.push_str("<tr>");
            for field in fields.clone() {
                let _ = write!(body, "<td>{}</td>", escape(&row.value(field)));
            }
            body.push_str("</tr>\n");
        }
        body.push_str("</table>\n");
        page("Execution report", &self.workflow, &body)
    }

    /// The graph of the processes, with the files between their tasks, drawn as SVG.
    fn dag(&self) -> String {
        let mut graph = ExecutionGraph {
            language: Some(Language::Nextflow),
            workflow: self.workflow.clone(),
            ..ExecutionGraph::default()
        };
        for id in &self.order {
            let Some((task, _)) = self.asked.get(id) else {
                continue;
            };
            for input in &task.inputs {
                let from = Path::new(&input.source)
                    .ancestors()
                    .find_map(|path| self.producers.get(&*path.to_string_lossy()));
                if let Some(from) = from.filter(|from| **from != task.id) {
                    graph.edges.push(Edge {
                        from: *from,
                        to: task.id,
                        kind: EdgeKind::File {
                            path: input.path.clone(),
                        },
                    });
                }
            }
            graph.nodes.push(Node {
                task: task.clone(),
                exit_code: self.exits.get(id).copied(),
                reused: false,
                markers: Vec::new(),
            });
        }
        let body = render(&graph, View::Steps, Format::Svg);
        page("Pipeline graph", &self.workflow, &body)
    }
}

impl Row {
    fn status(&self) -> &'static str {
        match (self.cached, self.exit) {
            (true, _) => "CACHED",
            (false, 0) => "COMPLETED",
            (false, _) => "FAILED",
        }
    }

    fn realtime(&self) -> Option<i64> {
        Some(self.complete? - self.start?)
    }

    /// The value of a trace field, as Nextflow writes it.
    fn value(&self, field: &str) -> String {
        let resources = &self.task.resources;
        match field {
            "task_id" => self.number.to_string(),
            "name" => self.task.name.clone(),
            "process" => self.task.step.clone(),
            "status" => self.status().to_owned(),
            "exit" => self.exit.to_string(),
            "attempt" => self.attempt.to_string(),
            "container" => (self.image.clone().or(self.task.image.clone()))
                .unwrap_or_else(|| UNKNOWN.to_owned()),
            "submit" => stamp(self.submit),
            "start" => self.start.map_or(UNKNOWN.to_owned(), stamp),
            "complete" => self.complete.map_or(UNKNOWN.to_owned(), stamp),
            "duration" => duration(self.complete.map(|complete| complete - self.submit)),
            "realtime" => duration(self.realtime()),
            "cpus" => (resources.cpus).map_or(UNKNOWN.to_owned(), |cpus| cpus.to_string()),
            "memory" => (resources.memory_bytes).map_or(UNKNOWN.to_owned(), size),
            "time" => (resources.time_seconds).map_or(UNKNOWN.to_owned(), |seconds| {
                duration(Some(seconds as i64 * 1000))
            }),
            _ => UNKNOWN.to_owned(),
        }
    }
}

/// A duration as Nextflow writes it: `850ms`, `3.4s`, `1m 5s`, `2h 1m 3s`.
fn duration(millis: Option<i64>) -> String {
    let Some(millis) = millis.filter(|millis| *millis >= 0) else {
        return UNKNOWN.to_owned();
    };
    if millis < 1000 {
        return format!("{millis}ms");
    }
    if millis < 60_000 {
        let tenths = (millis as f64 / 100.0).round() as i64;
        return match tenths % 10 {
            0 => format!("{}s", tenths / 10),
            rest => format!("{}.{rest}s", tenths / 10),
        };
    }
    let seconds = (millis as f64 / 1000.0).round() as i64;
    let parts = [
        (seconds / 86_400, "d"),
        (seconds / 3600 % 24, "h"),
        (seconds / 60 % 60, "m"),
        (seconds % 60, "s"),
    ];
    let words: Vec<String> = (parts.iter())
        .filter(|(value, _)| *value > 0)
        .map(|(value, unit)| format!("{value}{unit}"))
        .collect();
    words.join(" ")
}

/// A size as Nextflow writes it: `512 B`, `1.5 GB`.
fn size(bytes: u64) -> String {
    const UNITS: [&str; 7] = ["B", "KB", "MB", "GB", "TB", "PB", "EB"];
    if bytes == 0 {
        return "0".to_owned();
    }
    let group = ((bytes as f64).log2() / 10.0).floor() as usize;
    let value = bytes as f64 / 1024f64.powi(group as i32);
    let text = format!("{:.1}", value);
    let text = text.strip_suffix(".0").unwrap_or(&text);
    format!("{text} {}", UNITS[group.min(6)])
}

/// A time as Nextflow writes it in the trace, in the local time zone.
fn stamp(millis: i64) -> String {
    date::format(millis, "yyyy-MM-dd HH:mm:ss.SSS").unwrap_or_else(|_| UNKNOWN.to_owned())
}

/// The milliseconds since midnight of a time, in the local time zone.
fn millis_of_day(millis: i64) -> i64 {
    let clock = date::format(millis, "HH:mm:ss.SSS").unwrap_or_default();
    let numbers: Vec<i64> = (clock.split([':', '.']))
        .filter_map(|part| part.parse().ok())
        .collect();
    match numbers[..] {
        [hours, minutes, seconds, millis] => {
            ((hours * 60 + minutes) * 60 + seconds) * 1000 + millis
        }
        _ => 0,
    }
}

fn millis(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as i64)
}

fn now() -> i64 {
    millis(SystemTime::now())
}

/// A small HTML page that needs nothing from the network.
fn page(title: &str, workflow: &str, body: &str) -> String {
    format!(
        "<!DOCTYPE html>\n<html>\n<head>\n<meta charset=\"utf-8\">\n<title>{title}: {workflow}</title>\n\
         <style>body {{ font-family: sans-serif; margin: 2em; }} table {{ border-collapse: collapse; }} \
         td, th {{ border: 1px solid #ccc; padding: 2px 6px; text-align: left; }}</style>\n\
         </head>\n<body>\n<h1>{title}</h1>\n<p>{workflow}, run by iunctus.</p>\n{body}</body>\n</html>\n",
        workflow = escape(workflow),
    )
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_and_sizes_read_as_in_nextflow() {
        let cases = [
            (850, "850ms"),
            (3400, "3.4s"),
            (5000, "5s"),
            (65_000, "1m 5s"),
            (7_263_000, "2h 1m 3s"),
        ];
        for (millis, text) in cases {
            assert_eq!(duration(Some(millis)), text);
        }
        assert_eq!(duration(None), "-");
        assert_eq!(size(512), "512 B");
        assert_eq!(size(3 << 29), "1.5 GB");
    }
}
