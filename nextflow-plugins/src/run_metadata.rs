//! Nextflow's `workflow` metadata, run statistics and trace records, as the plugins that
//! report to network services send them.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;

use md5::{Digest, Md5};
use serde_json::Value as JsonValue;

use crate::observer::{End, Event, Start, Task};
use crate::report_format::{Json, NEXTFLOW_VERSION, now};

/// The build number and build time of Nextflow 26.04.6, as `workflow.nextflow` shows them.
pub(crate) const NEXTFLOW_BUILD: i64 = 12646;
pub(crate) const NEXTFLOW_TIMESTAMP: &str = "2026-07-09T18:49:00Z";

/// The fields of a Nextflow trace record, in the order of its JSON form.
pub(crate) const TRACE_FIELDS: [&str; 43] = [
    "task_id",
    "status",
    "hash",
    "name",
    "exit",
    "submit",
    "start",
    "process",
    "tag",
    "module",
    "container",
    "attempt",
    "script",
    "scratch",
    "workdir",
    "queue",
    "cpus",
    "memory",
    "disk",
    "time",
    "env",
    "accelerator",
    "accelerator_type",
    "error_action",
    "complete",
    "duration",
    "realtime",
    "%cpu",
    "cpu_model",
    "rchar",
    "wchar",
    "syscr",
    "syscw",
    "read_bytes",
    "write_bytes",
    "%mem",
    "vmem",
    "rss",
    "peak_vmem",
    "peak_rss",
    "vol_ctxt",
    "inv_ctxt",
    "native_id",
];

/// A field of a task's trace record; `module` is an empty list and other unknown fields null.
pub(crate) fn trace_value(task: &Task, field: &str) -> JsonValue {
    match task.record.get(field) {
        Some(value) => value.clone(),
        None if field == "module" => JsonValue::Array(Vec::new()),
        None => JsonValue::Null,
    }
}

/// A number field of a task's trace record.
pub(crate) fn trace_number(task: &Task, field: &str) -> Option<f64> {
    task.record.get(field).and_then(JsonValue::as_f64)
}

pub(crate) fn trace_text<'a>(task: &'a Task, field: &str) -> &'a str {
    (task.record.get(field))
        .and_then(JsonValue::as_str)
        .unwrap_or_default()
}

/// A run as the service plugins know it: its start, its statistics and how it ended.
pub(crate) struct Run {
    pub(crate) start: Start,
    /// The MD5 of the main script's text in hex, as Nextflow's `workflow.scriptId`.
    pub(crate) script_id: Option<String>,
    pub(crate) stats: Stats,
    pub(crate) ending: Option<Ending>,
}

/// How a run ended, as `workflow` shows it.
pub(crate) struct Ending {
    pub(crate) end: i64,
    pub(crate) success: bool,
    pub(crate) exit_status: i64,
    /// The first line of the failing task's standard error, else of the error.
    pub(crate) error_message: Option<String>,
    pub(crate) error_report: Option<String>,
}

impl Ending {
    pub(crate) fn of(end: &End) -> Ending {
        let exit = (end.task.as_ref())
            .and_then(|task| task.record.get("exit")?.as_i64())
            .filter(|exit| *exit != 0);
        let stderr = (end.task.as_ref())
            .and_then(|task| task.stderr.lines().find(|line| !line.trim().is_empty()))
            .map(|line| line.trim().to_owned());
        let first = (end.error.as_deref()).and_then(|error| error.lines().next());
        Ending {
            end: end.end,
            success: end.success,
            exit_status: match end.success {
                true => 0,
                false => exit.unwrap_or(1),
            },
            error_message: stderr.or(first.map(str::to_owned)),
            error_report: end.error.clone(),
        }
    }
}

impl Run {
    pub(crate) fn new(start: &Start) -> Run {
        let script_id = std::fs::read(&start.script).ok().map(|text| {
            Md5::digest(text)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect()
        });
        Run {
            stats: Stats::new(start),
            start: start.clone(),
            script_id,
            ending: None,
        }
    }

    pub(crate) fn end(&mut self, end: &End) {
        self.ending = Some(Ending::of(end));
        self.stats.ended = true;
    }

    /// Nextflow's `workflow` map without `stats`, with times as `time` writes them.
    pub(crate) fn workflow(&self, time: impl Fn(i64) -> Json) -> Json {
        let start = &self.start;
        let ending = self.ending.as_ref();
        let config = &start.config;
        let enabled = |scope: &str| config[scope]["enabled"].as_bool().unwrap_or(false);
        let engine = [
            "docker",
            "podman",
            "singularity",
            "apptainer",
            "charliecloud",
        ]
        .into_iter()
        .find(|engine| enabled(engine));
        let script_name = std::path::Path::new(&start.script)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
        let output = match config["outputDir"].as_str() {
            Some(output) => absolute(&start.launch_dir, output),
            None => format!("{}/results", start.launch_dir),
        };
        let words: Vec<&str> = start.command_line.split_whitespace().collect();
        let platform = ["workflowId", "workflowUrl", "user", "workspace"]
            .into_iter()
            .chain(["computeEnv", "pipeline", "labels", "schedRunId"])
            .map(|key| (key, Json::Null));
        let variable = |name: &str| std::env::var(name).unwrap_or_default();
        Json::object([
            ("runName", Json::from(&start.run_name)),
            ("scriptId", Json::from(self.script_id.clone())),
            ("scriptFile", Json::from(&start.script)),
            ("scriptName", Json::from(script_name)),
            ("repository", Json::from(start.repository.clone())),
            ("commitId", Json::from(start.commit_id.clone())),
            ("revision", Json::from(start.revision.clone())),
            ("start", time(start.start)),
            (
                "complete",
                ending.map_or(Json::Null, |ending| time(ending.end)),
            ),
            (
                "duration",
                Json::from(ending.map(|ending| ending.end - start.start)),
            ),
            (
                "container",
                Json::from(config["process"]["container"].as_str()),
            ),
            ("commandLine", Json::from(&start.command_line)),
            ("nextflow", nextflow()),
            (
                "success",
                Json::from(ending.is_some_and(|ending| ending.success)),
            ),
            ("projectDir", Json::from(&start.project_dir)),
            ("projectName", Json::from(&start.project_name)),
            ("launchDir", Json::from(&start.launch_dir)),
            ("outputDir", Json::from(output)),
            ("workDir", Json::from(&start.work_dir)),
            ("homeDir", Json::from(variable("HOME"))),
            ("userName", Json::from(variable("USER"))),
            (
                "exitStatus",
                Json::from(ending.map(|ending| ending.exit_status)),
            ),
            (
                "errorMessage",
                Json::from(ending.and_then(|ending| ending.error_message.clone())),
            ),
            (
                "errorReport",
                Json::from(ending.and_then(|ending| ending.error_report.clone())),
            ),
            ("profile", Json::from(&start.profile)),
            ("sessionId", Json::from(&start.session_id)),
            ("resume", Json::from(words.contains(&"-resume"))),
            ("stubRun", Json::from(words.contains(&"-stub-run"))),
            ("preview", Json::from(words.contains(&"-preview"))),
            ("containerEngine", Json::from(engine)),
            (
                "wave",
                Json::object([("enabled", Json::from(enabled("wave")))]),
            ),
            (
                "fusion",
                Json::object([
                    ("enabled", Json::from(enabled("fusion"))),
                    ("version", Json::Null),
                ]),
            ),
            ("platform", Json::object(platform)),
            ("configFiles", Json::list(start.config_files.iter())),
            ("manifest", manifest(&start.manifest)),
            (
                "failOnIgnore",
                Json::from(config["workflow"]["failOnIgnore"].as_bool() == Some(true)),
            ),
        ])
    }
}

/// `workflow.nextflow`: the version of Nextflow that uwe runs pipelines as.
fn nextflow() -> Json {
    Json::object([
        ("version", Json::from(NEXTFLOW_VERSION)),
        ("build", Json::from(NEXTFLOW_BUILD)),
        ("timestamp", Json::from(NEXTFLOW_TIMESTAMP)),
        ("enable", Json::object::<&str>([])),
    ])
}

/// `workflow.manifest` with Nextflow's keys: most values as text, `mainScript` `main.nf` by
/// default, and a list of Git submodules joined with commas.
fn manifest(manifest: &JsonValue) -> Json {
    let text = |key: &str| match &manifest[key] {
        JsonValue::Null => Json::Null,
        JsonValue::String(text) => Json::from(text),
        JsonValue::Array(items) if key == "gitmodules" => Json::from(
            (items.iter())
                .map(|item| item.as_str().map_or(item.to_string(), str::to_owned))
                .collect::<Vec<_>>()
                .join(","),
        ),
        other => Json::from(other.to_string()),
    };
    let contributors = match &manifest["contributors"] {
        JsonValue::Null => Json::list(Vec::<Json>::new()),
        other => Json::from(other),
    };
    let main = match text("mainScript") {
        Json::Null => Json::from("main.nf"),
        main => main,
    };
    let recurse = manifest["recurseSubmodules"].as_bool() == Some(true);
    Json::object([
        ("author", text("author")),
        ("contributors", contributors),
        ("defaultBranch", text("defaultBranch")),
        ("description", text("description")),
        ("docsUrl", text("docsUrl")),
        ("doi", text("doi")),
        ("gitmodules", text("gitmodules")),
        ("homePage", text("homePage")),
        ("icon", text("icon")),
        ("license", text("license")),
        ("mainScript", main),
        ("name", text("name")),
        ("nextflowVersion", text("nextflowVersion")),
        ("organization", text("organization")),
        ("recurseSubmodules", Json::from(recurse)),
        ("version", text("version")),
    ])
}

/// A path of the config made absolute against the launch directory.
fn absolute(launch: &str, path: &str) -> String {
    match path.starts_with('/') || path.contains("://") {
        true => path.to_owned(),
        false => format!("{launch}/{}", path.trim_start_matches("./")),
    }
}

/// The task counts of a run and of each process, as Nextflow's workflow statistics.
#[derive(Default)]
pub(crate) struct Stats {
    processes: Vec<Counter>,
    /// Where each task is: its process in `processes` and its last state.
    tasks: HashMap<i64, (usize, &'static str)>,
    total: Counter,
    /// When the counts last changed, in milliseconds since 1970; zero before.
    changed: i64,
    succeed_millis: i64,
    cached_millis: i64,
    failed_millis: i64,
    /// CPU milliseconds of the tasks that ran: run time by CPUs.
    cpu_millis: f64,
    pub(crate) ended: bool,
}

/// The counts of one process, or of the whole run.
#[derive(Clone, Default)]
struct Counter {
    name: String,
    hash: Option<String>,
    task_name: Option<String>,
    submitted: u64,
    running: u64,
    succeeded: u64,
    cached: u64,
    failed: u64,
    retries: u64,
    load_cpus: u64,
    load_memory: u64,
    peak_running: u64,
    peak_cpus: u64,
    peak_memory: u64,
}

impl Stats {
    /// The counts of a run with the processes of its start.
    pub(crate) fn new(start: &Start) -> Stats {
        let processes = (start.processes.iter())
            .map(|process| Counter {
                name: process.name.clone(),
                ..Counter::default()
            })
            .collect();
        Stats {
            processes,
            ..Stats::default()
        }
    }

    /// Counts a task event; other events change nothing.
    pub(crate) fn count(&mut self, event: &Event) {
        let (task, state) = match event {
            Event::TaskSubmitted(task) => (task, "submitted"),
            Event::TaskStarted(task) => (task, "running"),
            Event::TaskCompleted(task) => (task, "succeeded"),
            Event::TaskCached(task) => (task, "cached"),
            Event::TaskFailed(task) => (task, "failed"),
            _ => return,
        };
        let id = trace_number(task, "task_id").unwrap_or_default() as i64;
        let process = trace_text(task, "process");
        let index = match self.processes.iter().position(|item| item.name == process) {
            Some(index) => index,
            None => {
                self.processes.push(Counter {
                    name: process.to_owned(),
                    ..Counter::default()
                });
                self.processes.len() - 1
            }
        };
        let cpus = trace_number(task, "cpus").unwrap_or(1.0) as u64;
        let memory = trace_number(task, "memory").unwrap_or_default() as u64;
        let realtime = trace_number(task, "realtime").unwrap_or_default() as i64;
        let previous = self
            .tasks
            .insert(id, (index, state))
            .map(|(_, state)| state);
        for counter in [&mut self.processes[index], &mut self.total] {
            counter.leave(previous, state, cpus, memory);
            counter.enter(state, cpus, memory);
            if state != "submitted" && state != "running" {
                counter.hash = Some(trace_text(task, "hash").to_owned());
                counter.task_name = Some(trace_text(task, "name").to_owned());
            }
        }
        match state {
            "succeeded" => self.succeed_millis += realtime,
            "cached" => self.cached_millis += realtime,
            "failed" => self.failed_millis += realtime,
            _ => {}
        }
        if matches!(state, "succeeded" | "failed") {
            self.cpu_millis += realtime as f64 * cpus as f64;
        }
        self.changed = now();
    }

    /// The counts as nf-tower's `progress` object.
    pub(crate) fn progress(&self) -> Json {
        let total = &self.total;
        Json::object([
            ("aborted", Json::from(0u64)),
            ("cached", Json::from(total.cached)),
            ("failed", Json::from(total.failed)),
            ("ignored", Json::from(0u64)),
            ("loadCpus", Json::from(total.load_cpus)),
            ("loadMemory", Json::from(total.load_memory)),
            ("peakCpus", Json::from(total.peak_cpus)),
            ("peakMemory", Json::from(total.peak_memory)),
            ("peakRunning", Json::from(total.peak_running)),
            ("pending", Json::from(0u64)),
            ("processes", self.process_list(false)),
            ("retries", Json::from(total.retries)),
            ("running", Json::from(total.running)),
            ("submitted", Json::from(total.submitted)),
            ("succeeded", Json::from(total.succeeded)),
        ])
    }

    /// The counts as `workflow.stats`; `detailed` adds the fields of each process that
    /// nf-weblog sends.
    pub(crate) fn workflow(&self, detailed: bool) -> Json {
        let total = &self.total;
        let count = total.succeeded + total.cached + total.failed;
        let percent = |part: u64| match count {
            0 => 0.0,
            _ => (part as f64 * 1000.0 / count as f64).round() / 10.0,
        };
        let hours = self.cpu_millis / 3_600_000.0;
        let compute = match hours < 0.1 {
            true => "(a few seconds)".to_owned(),
            false => format!("{hours:.1}"),
        };
        Json::object([
            ("changeTimestamp", Json::from(self.changed)),
            ("succeededCount", Json::from(total.succeeded)),
            ("cachedCount", Json::from(total.cached)),
            ("failedCount", Json::from(total.failed)),
            ("ignoredCount", Json::from(0u64)),
            ("pendingCount", Json::from(0u64)),
            ("submittedCount", Json::from(total.submitted)),
            ("runningCount", Json::from(total.running)),
            ("retriesCount", Json::from(total.retries)),
            ("abortedCount", Json::from(0u64)),
            ("loadCpus", Json::from(total.load_cpus)),
            ("loadMemory", Json::from(total.load_memory)),
            ("peakRunning", Json::from(total.peak_running)),
            ("peakCpus", Json::from(total.peak_cpus)),
            ("peakMemory", Json::from(total.peak_memory)),
            ("cachedCountFmt", Json::from(grouped(total.cached))),
            ("cachedDuration", Json::from(self.cached_millis)),
            ("cachedPct", Json::from(percent(total.cached))),
            ("computeTimeFmt", Json::from(compute)),
            ("effectiveFailedCount", Json::from(total.failed)),
            ("effectiveFailedPct", Json::from(percent(total.failed))),
            ("failedCountFmt", Json::from(grouped(total.failed))),
            ("failedDuration", Json::from(self.failed_millis)),
            ("ignoredCountFmt", Json::from("0")),
            ("ignoredPct", Json::from(0.0)),
            ("loadMemoryFmt", Json::from(memory_text(total.load_memory))),
            ("peakMemoryFmt", Json::from(memory_text(total.peak_memory))),
            ("processes", self.process_list(detailed)),
            ("progressLength", Json::from(self.processes.len() as u64)),
            ("succeedCount", Json::from(total.succeeded)),
            ("succeedCountFmt", Json::from(grouped(total.succeeded))),
            ("succeedDuration", Json::from(self.succeed_millis)),
            ("succeedPct", Json::from(percent(total.succeeded))),
            ("totalCount", Json::from(count)),
        ])
    }

    /// The counts of each process. Their tasks' work directories are unknown to uwe.
    fn process_list(&self, detailed: bool) -> Json {
        let entries = self.processes.iter().enumerate().map(|(index, counter)| {
            let mut entries = vec![
                ("index", Json::from(index as u64 + 1)),
                ("name", Json::from(&counter.name)),
            ];
            if detailed {
                entries.push(("hash", Json::from(counter.hash.clone())));
            }
            entries.push(("workDir", Json::Null));
            if detailed {
                entries.push(("taskName", Json::from(counter.task_name.clone())));
            }
            entries.extend([
                ("pending", Json::from(0u64)),
                ("submitted", Json::from(counter.submitted)),
                ("running", Json::from(counter.running)),
                ("succeeded", Json::from(counter.succeeded)),
                ("cached", Json::from(counter.cached)),
                ("failed", Json::from(counter.failed)),
                ("aborted", Json::from(0u64)),
                ("stored", Json::from(0u64)),
                ("ignored", Json::from(0u64)),
                ("retries", Json::from(counter.retries)),
                ("terminated", Json::from(self.ended)),
            ]);
            if detailed {
                entries.push(("errored", Json::from(counter.failed > 0)));
            }
            entries.extend([
                ("loadCpus", Json::from(counter.load_cpus)),
                ("loadMemory", Json::from(counter.load_memory)),
                ("peakRunning", Json::from(counter.peak_running)),
                ("peakCpus", Json::from(counter.peak_cpus)),
                ("peakMemory", Json::from(counter.peak_memory)),
            ]);
            if detailed {
                let done = counter.succeeded + counter.cached + counter.failed;
                entries.push(("completedCount", Json::from(done)));
                let all = done + counter.submitted + counter.running;
                entries.push(("totalCount", Json::from(all)));
            }
            Json::object(entries)
        });
        Json::List(entries.collect())
    }
}

impl Counter {
    /// Takes a task out of the state it was in; a failed task that is submitted again is a
    /// retry.
    fn leave(&mut self, previous: Option<&str>, next: &str, cpus: u64, memory: u64) {
        match previous {
            Some("submitted") => self.submitted = self.submitted.saturating_sub(1),
            Some("running") => {
                self.running = self.running.saturating_sub(1);
                self.load_cpus = self.load_cpus.saturating_sub(cpus);
                self.load_memory = self.load_memory.saturating_sub(memory);
            }
            Some("failed") if next == "submitted" => {
                self.failed = self.failed.saturating_sub(1);
                self.retries += 1;
            }
            _ => {}
        }
    }

    fn enter(&mut self, state: &str, cpus: u64, memory: u64) {
        match state {
            "submitted" => self.submitted += 1,
            "running" => {
                self.running += 1;
                self.load_cpus += cpus;
                self.load_memory += memory;
                self.peak_running = self.peak_running.max(self.running);
                self.peak_cpus = self.peak_cpus.max(self.load_cpus);
                self.peak_memory = self.peak_memory.max(self.load_memory);
            }
            "succeeded" => self.succeeded += 1,
            "cached" => self.cached += 1,
            _ => self.failed += 1,
        }
    }
}

/// A count with thousands separated by commas, as `%,d` prints it.
fn grouped(count: u64) -> String {
    let digits = count.to_string();
    let mut text = String::new();
    for (place, digit) in digits.chars().enumerate() {
        if place > 0 && (digits.len() - place) % 3 == 0 {
            text.push(',');
        }
        text.push(digit);
    }
    text
}

fn memory_text(bytes: u64) -> String {
    nextflow_evaluator::units::memory_text(i64::try_from(bytes).unwrap_or(i64::MAX))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::observer::Task;

    /// The start of a run in `/launch` with one process, for tests.
    pub(crate) fn start() -> Start {
        Start {
            run_name: "lethal_legentil".into(),
            session_id: "6528c046-1ce8-4848-b491-ee3e024c7752".into(),
            launch_dir: "/launch".into(),
            work_dir: "/launch/work".into(),
            project_dir: "/launch".into(),
            script: "/launch/main.nf".into(),
            project_name: "main.nf".into(),
            repository: None,
            commit_id: None,
            revision: None,
            command_line: "uwe run main.nf".into(),
            profile: "standard".into(),
            config_files: vec!["/launch/nextflow.config".into()],
            params: serde_json::json!({"greeting": "hello"}),
            config: serde_json::json!({}),
            manifest: serde_json::json!({"name": "demo/probe"}),
            processes: Vec::new(),
            start: 1_793_965_381_122,
        }
    }

    fn task(id: i64, process: &str, realtime: i64) -> Task {
        let record = serde_json::json!({
            "task_id": id, "process": process, "name": format!("{process} ({id})"),
            "hash": "ab/cdef01", "cpus": 2, "realtime": realtime,
        });
        Task {
            record: record.as_object().cloned().unwrap_or_default(),
            process_id: None,
            hash: String::new(),
            executor: "local".into(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            values: Vec::new(),
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    #[test]
    fn counts_task_states() {
        let mut stats = Stats::default();
        stats.count(&Event::TaskSubmitted(task(1, "SAY", 0)));
        stats.count(&Event::TaskSubmitted(task(2, "SAY", 0)));
        stats.count(&Event::TaskStarted(task(1, "SAY", 0)));
        stats.count(&Event::TaskStarted(task(2, "SAY", 0)));
        stats.count(&Event::TaskFailed(task(1, "SAY", 5)));
        stats.count(&Event::TaskSubmitted(task(1, "SAY", 0)));
        stats.count(&Event::TaskCompleted(task(2, "SAY", 7)));
        let progress = stats.progress();
        let field = |name: &str| progress.get(name).cloned();
        assert_eq!(field("submitted"), Some(Json::from(1u64)));
        assert_eq!(field("succeeded"), Some(Json::from(1u64)));
        assert_eq!(field("failed"), Some(Json::from(0u64)));
        assert_eq!(field("retries"), Some(Json::from(1u64)));
        assert_eq!(field("peakRunning"), Some(Json::from(2u64)));
        assert_eq!(field("peakCpus"), Some(Json::from(4u64)));
        let workflow = stats.workflow(false);
        assert_eq!(workflow.get("succeedDuration"), Some(&Json::from(7i64)));
        assert_eq!(workflow.get("succeedPct"), Some(&Json::from(100.0)));
    }

    #[test]
    fn groups_thousands() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(1234567), "1,234,567");
        assert_eq!(grouped(123), "123");
    }
}
