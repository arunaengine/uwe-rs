//! Writes configured trace, timeline, summary, and process graph reports after real runs, and
//! tells run observers the events of the run with the trace records of its tasks.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use nextflow_config::Config;
use nextflow_evaluator::date;
use nextflow_evaluator::value::Value;
use nextflow_plugins::observer::{self, Counts, End, Event, Observer, Record, Start};
use workflow_graph::render::{Format, Options, View, render};
use workflow_graph::{
    Delivery, DeliveryMode, Edge, EdgeKind, ExecutionGraph, Language, Node, Outcome, Task, TaskId,
    Usage,
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
    /// The run observers of plugins and of the caller, which hear each event of the run.
    observers: Vec<Box<dyn Observer>>,
    counts: Counts,
    /// How many attempts of each task the run asked for, for the observers.
    attempts: HashMap<TaskId, u32>,
    /// What the observers hear of each task besides its trace row.
    details: HashMap<TaskId, Details>,
    /// The number of each process, by its qualified name.
    process_ids: HashMap<String, u32>,
    /// The last failed attempt of each task, for the end of a run that a task failed.
    failures: HashMap<TaskId, observer::Task>,
    /// Whether the observers heard the end of the run, after which they hear nothing.
    closed: bool,
    /// Whether nf-cgroup-metrics fills the memory fields from the samples of the cgroup.
    cgroup: bool,
}

/// What observers hear of a task besides its trace row, from the request of the task.
struct Details {
    hash: String,
    tag: Option<String>,
    queue: Option<String>,
    executor: String,
    inputs: Vec<observer::File>,
    values: Vec<serde_json::Value>,
}

impl Details {
    fn of(task: &Task, request: &crate::TaskRequest) -> Details {
        let directive = |name: &str| {
            let found = request
                .directives
                .iter()
                .rev()
                .find(|directive| &*directive.name == name);
            let value = found.and_then(|directive| directive.arguments.first());
            value.filter(|value| value.truth()).map(Value::to_string)
        };
        let states = run_database::input_states(task);
        let hash = run_database::fingerprint(task, &states);
        let inputs = (request.staged.iter())
            .map(|(path, name)| observer::File {
                name: name.clone(),
                path: path.uri(),
                digest: None,
            })
            .collect();
        Details {
            hash: hash.strip_prefix("sha256:").unwrap_or(&hash).to_owned(),
            tag: directive("tag"),
            queue: directive("queue"),
            executor: directive("executor").unwrap_or_else(|| "local".to_owned()),
            inputs,
            values: request.values.iter().map(crate::inputs::to_json).collect(),
        }
    }
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
    usage: Option<Usage>,
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
            observers: Vec::new(),
            counts: Counts::default(),
            attempts: HashMap::new(),
            details: HashMap::new(),
            process_ids: HashMap::new(),
            failures: HashMap::new(),
            closed: false,
            cgroup: nextflow_plugins::cgroup_metrics(config),
        }
    }

    /// Adds run observers, which hear the events from now on.
    pub fn observe(&mut self, observers: Vec<Box<dyn Observer>>) {
        self.observers.extend(observers);
    }

    pub fn is_empty(&self) -> bool {
        self.wanted.is_empty() && self.observers.is_empty()
    }

    /// Tells the observers that the run starts, with its options, its processes and where
    /// the pipeline came from.
    pub fn run_started(
        &mut self,
        options: &crate::Options,
        launch_dir: &Path,
        processes: Vec<observer::Process>,
        repository: Option<&crate::driver::Repository>,
    ) {
        if self.observers.is_empty() {
            return;
        }
        self.process_ids = (processes.iter())
            .map(|process| (process.name.clone(), process.id))
            .collect();
        let folder = Path::new(&options.project_dir).file_name();
        let folder = folder.map(|name| name.to_string_lossy().into_owned());
        let manifest = options.config.get(&["manifest"]);
        let start = Start {
            run_name: options.run_name.clone(),
            session_id: options.run_name.clone(),
            launch_dir: launch_dir.to_string_lossy().into_owned(),
            work_dir: launch_dir.join("work").to_string_lossy().into_owned(),
            project_dir: options.project_dir.clone(),
            script: options.script_file.clone(),
            project_name: (repository.map(|repository| repository.name.clone()))
                .or(folder)
                .unwrap_or_default(),
            repository: repository.map(|repository| repository.url.clone()),
            commit_id: repository.and_then(|repository| repository.commit_id.clone()),
            revision: repository.and_then(|repository| repository.revision.clone()),
            command_line: options.command_line.clone(),
            profile: options.profile.clone(),
            config_files: options.config_files.clone(),
            params: crate::inputs::to_json(&options.config.params()),
            config: crate::inputs::to_json(&options.config.tree),
            manifest: manifest.map_or(serde_json::json!({}), |manifest| {
                crate::inputs::to_json(&manifest)
            }),
            processes,
            start: self.started,
            resume: options.resume,
        };
        self.notify(Event::RunStarted(Box::new(start)));
    }

    /// Tells the observers that a file went to where the workflow publishes it.
    pub fn published(&mut self, delivery: &Delivery) {
        self.notify(Event::FilePublished {
            source: delivery.location.clone(),
            target: delivery.target.clone(),
        });
    }

    /// Lets each observer check a run that succeeded; the first error fails the run.
    pub fn check(&mut self) -> Result<(), String> {
        let mut first = Ok(());
        for observer in &mut self.observers {
            let checked = observer.check();
            if first.is_ok() {
                first = checked;
            }
        }
        first
    }

    /// Tells the observers that the run ended, with the message of the error that failed it
    /// and the task whose failure ended it, and with the workflow outputs.
    pub fn run_ended(
        &mut self,
        error: Option<&str>,
        failing: Option<crate::TaskId>,
        outputs: Vec<observer::Output>,
    ) {
        let task = failing.and_then(|task| self.failures.remove(&TaskId(task.0)));
        self.notify(Event::RunEnded(End {
            success: error.is_none(),
            error: error.map(str::to_owned),
            counts: self.counts,
            task,
            outputs,
            end: now(),
        }));
        self.closed = true;
    }

    /// Notes that the run asked for a task, or for another attempt of it.
    pub fn asked(&mut self, task: &Task, request: &crate::TaskRequest) {
        if !self.asked.contains_key(&task.id) {
            self.order.push(task.id);
        }
        let submit = now();
        self.asked.insert(task.id, (task.clone(), submit));
        if self.observers.is_empty() {
            return;
        }
        self.details.insert(task.id, Details::of(task, request));
        let attempt = self.attempts.entry(task.id).or_default();
        *attempt += 1;
        let row = Row {
            number: self.order.len(),
            task: task.clone(),
            exit: 0,
            attempt: *attempt,
            cached: false,
            image: None,
            submit,
            start: None,
            complete: None,
            usage: None,
        };
        let record = unfinished(row.record(), "SUBMITTED");
        self.notify(Event::TaskSubmitted(self.task(record, task.id, None)));
    }

    /// Tells the observers that an attempt of a task began.
    pub fn started(&mut self, id: TaskId, attempt: u32) {
        let Some((task, submit)) = self.asked.get(&id).filter(|_| !self.observers.is_empty())
        else {
            return;
        };
        let number = self.order.iter().position(|asked| *asked == id);
        let row = Row {
            number: number.map_or(0, |place| place + 1),
            task: task.clone(),
            exit: 0,
            attempt,
            cached: false,
            image: None,
            submit: *submit,
            start: Some(now()),
            complete: None,
            usage: None,
        };
        let record = unfinished(row.record(), "RUNNING");
        self.notify(Event::TaskStarted(self.task(record, id, None)));
    }

    /// Notes how an attempt of a task ended.
    pub fn ended(&mut self, outcome: &Outcome) {
        let Some((task, submit)) = self.asked.get(&outcome.id) else {
            return;
        };
        let number = self.order.iter().position(|id| *id == outcome.id);
        let row = Row {
            number: number.map_or(0, |place| place + 1),
            task: task.clone(),
            exit: outcome.exit_code,
            attempt: outcome.attempt,
            cached: outcome.cached,
            image: outcome.image.clone(),
            submit: *submit,
            start: outcome.started.map(millis),
            complete: outcome.ended.map(millis),
            usage: outcome.usage.map(|usage| memory(usage, self.cgroup)),
        };
        if !self.observers.is_empty() {
            self.tell(&row, outcome);
        }
        self.rows.push(row);
        self.exits.insert(outcome.id, outcome.exit_code);
        for file in &outcome.files {
            self.producers.insert(file.location.clone(), outcome.id);
        }
    }

    /// Tells the observers how an attempt ended.
    fn tell(&mut self, row: &Row, outcome: &Outcome) {
        let task = self.task(row.record(), outcome.id, Some(outcome));
        let event = match row.status() {
            "CACHED" => {
                self.counts.cached += 1;
                Event::TaskCached(task)
            }
            "COMPLETED" => {
                self.counts.succeeded += 1;
                Event::TaskCompleted(task)
            }
            _ => {
                self.counts.failed += 1;
                self.failures.insert(outcome.id, task.clone());
                Event::TaskFailed(task)
            }
        };
        self.notify(event);
    }

    /// A task for the observers: its record with the details of its request, and the files
    /// of its outcome once it ended.
    fn task(&self, mut record: Record, id: TaskId, outcome: Option<&Outcome>) -> observer::Task {
        let Some(details) = self.details.get(&id) else {
            return observer::Task {
                record,
                process_id: None,
                hash: String::new(),
                executor: "local".to_owned(),
                inputs: Vec::new(),
                outputs: Vec::new(),
                values: Vec::new(),
                stdout: String::new(),
                stderr: String::new(),
            };
        };
        let short = details
            .hash
            .get(..8)
            .map(|hex| format!("{}/{}", &hex[..2], &hex[2..]));
        record.insert("hash".into(), short.into());
        record.insert("tag".into(), details.tag.clone().into());
        record.insert("queue".into(), details.queue.clone().into());
        let mut inputs = details.inputs.clone();
        let mut outputs = Vec::new();
        if let Some(outcome) = outcome {
            for input in &mut inputs {
                input.digest = outcome.inputs.get(&input.name).cloned();
            }
            outputs = (outcome.files.iter())
                .map(|file| observer::File {
                    name: file.path.clone(),
                    path: file.location.clone(),
                    digest: file.digest.clone(),
                })
                .collect();
        }
        observer::Task {
            process_id: self.process_ids.get(&self.asked[&id].0.step).copied(),
            record,
            hash: details.hash.clone(),
            executor: details.executor.clone(),
            inputs,
            outputs,
            values: details.values.clone(),
            stdout: outcome
                .map(|outcome| outcome.stdout.clone())
                .unwrap_or_default(),
            stderr: outcome
                .map(|outcome| outcome.stderr.clone())
                .unwrap_or_default(),
        }
    }

    fn notify(&mut self, event: Event) {
        if self.closed {
            return;
        }
        for observer in &mut self.observers {
            observer.notify(&event);
        }
    }

    /// Writes the reports into `folder`, and gives their deliveries to their files.
    pub fn write(&self, folder: &Path) -> std::io::Result<Vec<Delivery>> {
        if self.wanted.is_empty() {
            return Ok(Vec::new());
        }
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
                target: target.to_string_lossy().into_owned(),
                mode: DeliveryMode::Copy,
                overwrite: true,
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
                accepted: None,
                reused: false,
                markers: Vec::new(),
            });
        }
        let steps = Options {
            view: View::Steps,
            ..Options::default()
        };
        let body = render(&graph, &steps, Format::Svg);
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

    /// A size that the executor measured, as Nextflow writes sizes.
    fn measured(&self, field: fn(&Usage) -> Option<u64>) -> String {
        (self.usage.as_ref().and_then(field)).map_or(UNKNOWN.to_owned(), size)
    }

    fn realtime(&self) -> Option<i64> {
        Some(self.complete? - self.start?)
    }

    /// The CPU time over the time of the task, as a share of one CPU in percent.
    fn cpu_share(&self) -> Option<f64> {
        let cpu = self.usage?.cpu_usec?;
        let real = self.realtime().filter(|real| *real > 0)?;
        Some(cpu as f64 / 10.0 / real as f64)
    }

    /// The share of the machine's memory that the largest anonymous memory of the cgroup took,
    /// in percent with one decimal, as nf-cgroup-metrics counts it in thousandths.
    fn memory_share(&self) -> Option<f64> {
        let usage = self.usage?;
        let total = usage.memory_total? / 1024;
        let thousandths = (usage.rss? / 1024) as f64 * 1000.0 / total.max(1) as f64;
        Some(thousandths.round() / 10.0)
    }

    /// The trace record of the attempt for run observers, with the values unformatted.
    fn record(&self) -> Record {
        let resources = &self.task.resources;
        let usage = self.usage.as_ref();
        let record = serde_json::json!({
            "task_id": self.number,
            "name": self.task.name,
            "process": self.task.step,
            "status": self.status(),
            "exit": self.exit,
            "attempt": self.attempt,
            "container": self.image.as_ref().or(self.task.image.as_ref()),
            "submit": self.submit,
            "start": self.start,
            "complete": self.complete,
            "duration": self.complete.map(|complete| complete - self.submit),
            "realtime": self.realtime(),
            // Nextflow's `task.cpus` is 1 without a directive.
            "cpus": resources.cpus.unwrap_or(1.0),
            "memory": resources.memory_bytes,
            "time": resources.time_seconds.map(|seconds| seconds.saturating_mul(1000)),
            "%cpu": self.cpu_share(),
            "peak_rss": usage.and_then(|usage| usage.memory_peak),
            "read_bytes": usage.and_then(|usage| usage.read_bytes),
            "write_bytes": usage.and_then(|usage| usage.write_bytes),
            "disk": resources.disk_bytes,
            "script": self.task.script,
            // What uwe does not know or measure: executors keep task folders and job ids to
            // themselves, and count no character or context-switch figures.
            "workdir": null,
            "native_id": null,
            "cpu_model": null,
            "rchar": null,
            "wchar": null,
            "rss": usage.and_then(|usage| usage.rss),
            "vmem": usage.and_then(|usage| usage.vmem),
            "peak_vmem": usage.and_then(|usage| usage.peak_vmem),
            "%mem": self.memory_share(),
            "vol_ctxt": null,
            "inv_ctxt": null,
        });
        match record {
            serde_json::Value::Object(record) => record,
            _ => Record::new(),
        }
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
            "cpus" => resources.cpus.unwrap_or(1.0).to_string(),
            "memory" => (resources.memory_bytes).map_or(UNKNOWN.to_owned(), size),
            "time" => (resources.time_seconds).map_or(UNKNOWN.to_owned(), |seconds| {
                duration(Some(seconds as i64 * 1000))
            }),
            "%cpu" => (self.cpu_share()).map_or(UNKNOWN.to_owned(), |share| format!("{share:.1}%")),
            // The peak of the container's memory, page cache included.
            "peak_rss" => self.measured(|usage| usage.memory_peak),
            "read_bytes" => self.measured(|usage| usage.read_bytes),
            "write_bytes" => self.measured(|usage| usage.write_bytes),
            "vmem" => self.measured(|usage| usage.vmem),
            "rss" => self.measured(|usage| usage.rss),
            "peak_vmem" => self.measured(|usage| usage.peak_vmem),
            "%mem" => {
                (self.memory_share()).map_or(UNKNOWN.to_owned(), |share| format!("{share:.1}%"))
            }
            _ => UNKNOWN.to_owned(),
        }
    }
}

/// The usage of an attempt as the trace shows it. With nf-cgroup-metrics, the samples of the
/// cgroup count in whole KiB, and `peak_rss` is the largest anonymous memory; else they stay out.
fn memory(mut usage: Usage, cgroup: bool) -> Usage {
    let whole = |bytes: Option<u64>| bytes.map(|bytes| bytes / 1024 * 1024);
    if !cgroup {
        return Usage {
            vmem: None,
            rss: None,
            peak_vmem: None,
            memory_total: None,
            ..usage
        };
    }
    usage.vmem = whole(usage.vmem);
    usage.rss = whole(usage.rss);
    usage.peak_vmem = whole(usage.peak_vmem);
    if usage.rss.is_some() {
        usage.memory_peak = usage.rss;
    }
    usage
}

/// The record of an attempt that has not ended, with this status and without the values of
/// its end.
fn unfinished(mut record: Record, status: &str) -> Record {
    record.insert("status".into(), status.into());
    let ending = [
        "exit",
        "complete",
        "duration",
        "realtime",
        "%cpu",
        "peak_rss",
        "read_bytes",
        "write_bytes",
    ];
    for field in ending {
        record.insert(field.into(), serde_json::Value::Null);
    }
    if status == "SUBMITTED" {
        record.insert("start".into(), serde_json::Value::Null);
    }
    record
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
         </head>\n<body>\n<h1>{title}</h1>\n<p>{workflow}, run by uwe.</p>\n{body}</body>\n</html>\n",
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
    fn reports_measured_usage() {
        let row = Row {
            number: 1,
            task: Task::default(),
            exit: 0,
            attempt: 1,
            cached: false,
            image: None,
            submit: 0,
            start: Some(1000),
            complete: Some(3000),
            usage: Some(Usage {
                cpu_usec: Some(1_000_000),
                memory_peak: Some(3 << 20),
                read_bytes: Some(0),
                write_bytes: None,
                ..Usage::default()
            }),
        };
        assert_eq!(row.value("%cpu"), "50.0%");
        assert_eq!(row.value("peak_rss"), "3 MB");
        assert_eq!(row.value("read_bytes"), "0");
        assert_eq!(row.value("write_bytes"), "-");
        let unmeasured = Row { usage: None, ..row };
        assert_eq!(unmeasured.value("%cpu"), "-");
    }

    /// Without directives, Nextflow 26.04.6 traces 1 CPU, and no memory, time or disk.
    #[test]
    fn fills_default_cpus() {
        let row = Row {
            number: 1,
            task: Task::default(),
            exit: 0,
            attempt: 1,
            cached: false,
            image: None,
            submit: 0,
            start: None,
            complete: None,
            usage: None,
        };
        let fields = ["attempt", "cpus", "memory", "time", "disk"];
        let values: Vec<String> = fields.iter().map(|field| row.value(field)).collect();
        assert_eq!(values, ["1", "1", "-", "-", "-"]);
        let record = row.record();
        assert_eq!(record["cpus"], serde_json::json!(1.0));
        assert!(record["memory"].is_null() && record["time"].is_null());
    }

    #[test]
    fn parses_report_units() {
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
