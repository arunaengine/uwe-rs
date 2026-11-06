//! nf-gadi 1.4.0: a CSV or JSON report of the service units and resources of the PBS jobs of a
//! run on NCI Gadi, from the job summaries that the PBS executor adds to the tasks' stderr.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::Write as _;
use std::path::{Path, PathBuf};

use nextflow_config::Config;
use nextflow_evaluator::value::Value;

use crate::catalogue::{Kind, Plugin, State};
use crate::observer::{Event, Observer, Start, Task};
use crate::report_format::Json;

pub(crate) const PLUGIN: Plugin = Plugin {
    functions: &["sayHello"],
    call: Some(crate::nf_storagemetrics::say_hello),
    scopes: &["gadi"],
    observer: Some(activate),
    ..Plugin::new("nf-gadi", Kind::Observer, State::Partial)
};

/// The header of the CSV report.
const HEADER: &str = "Name,Process,Queue,Service Units,CPUs,CPU time,Used Walltime,\
                      Requested Walltime,Used Memory,Requested Memory,Used JobFS,\
                      Requested JobFS,Efficiency,Exit Code";

/// The fields of an entry, in the order of the JSON report, and the order of the CSV columns
/// after the name, process and queue.
const FIELDS: [&str; 11] = [
    "sus",
    "walltime",
    "cputime",
    "cpus",
    "memory",
    "efficiency",
    "exitCode",
    "requestedMemory",
    "requestedWalltime",
    "requestedJobFS",
    "usedJobFS",
];
const COLUMNS: [&str; 11] = [
    "sus",
    "cpus",
    "cputime",
    "walltime",
    "requestedWalltime",
    "memory",
    "requestedMemory",
    "usedJobFS",
    "requestedJobFS",
    "efficiency",
    "exitCode",
];

fn activate(config: &Config) -> Result<Option<Box<dyn Observer>>, String> {
    let text = |key: &str| match config.get(&["gadi", key]) {
        None | Some(Value::Null) => None,
        Some(value) => Some(value.to_string()),
    };
    let publish = match config.get(&["gadi", "publishTo"]) {
        None | Some(Value::Null) => None,
        Some(Value::String(path)) => Some(path.to_string()).filter(|path| !path.is_empty()),
        Some(_) => {
            tracing::warn!("uwe takes gadi.publishTo only as a path, not as a closure");
            None
        }
    };
    let (json, output) = settle(text("format").as_deref(), text("output").as_deref());
    Ok(Some(Box::new(Gadi {
        json,
        output: PathBuf::from(output),
        publish,
        launch: PathBuf::new(),
        report: Vec::new(),
    })))
}

/// The format of the report, JSON or else CSV, and its file, from `gadi.format` and
/// `gadi.output`, as nf-gadi settles them, with its log line.
fn settle(format: Option<&str>, output: Option<&str>) -> (bool, String) {
    let ending = |text: &str| text.rsplit('.').next().unwrap_or_default().to_owned();
    let (json, output, line) = match (format, output) {
        (None, None) => (
            false,
            None,
            "Using default report name UsageReport.csv".to_owned(),
        ),
        (Some(format), Some(output)) if ending(output) == format => (
            format == "json",
            Some(output),
            format!("Save the report to {output}"),
        ),
        (Some(_), Some(output)) if matches!(ending(output).as_str(), "json" | "csv") => {
            let ending = ending(output);
            let line = format!("Incorrect format setting, change it to {ending}");
            (ending == "json", Some(output), line)
        }
        (Some(_), Some(_)) => (
            false,
            None,
            "Incorrect format and output settings, reset \
                                           report name to UsageReport.csv"
                .to_owned(),
        ),
        (Some("json"), None) => (
            true,
            None,
            "Using default json report name UsageReport.json".to_owned(),
        ),
        (Some(_), None) => (
            false,
            None,
            "Using default csv report name UsageReport.csv".to_owned(),
        ),
        (None, Some(output)) if matches!(ending(output).as_str(), "json" | "csv") => {
            let ending = ending(output);
            (
                ending == "json",
                Some(output),
                format!("Report format is {ending}"),
            )
        }
        (None, Some(_)) => (
            false,
            None,
            "Incorrect output setting, reset report name to UsageReport.csv".to_owned(),
        ),
    };
    tracing::info!("{line}");
    let output = output.map_or_else(
        || format!("UsageReport.{}", if json { "json" } else { "csv" }),
        str::to_owned,
    );
    (json, output)
}

struct Gadi {
    json: bool,
    /// The report file, relative to the launch directory.
    output: PathBuf,
    publish: Option<String>,
    launch: PathBuf,
    /// The entries of a JSON report, by process in the order of their first entry.
    report: Vec<(String, Vec<Json>)>,
}

impl Observer for Gadi {
    fn notify(&mut self, event: &Event) {
        match event {
            Event::RunStarted(start) => self.started(start),
            Event::TaskCompleted(task) | Event::TaskFailed(task) | Event::TaskCached(task) => {
                self.add(task)
            }
            Event::RunEnded(_) => self.ended(),
            _ => {}
        }
    }
}

impl Gadi {
    fn path(&self) -> PathBuf {
        self.launch.join(&self.output)
    }

    /// The CSV report starts with its header, replacing a report of an earlier run.
    fn started(&mut self, start: &Start) {
        self.launch = PathBuf::from(&start.launch_dir);
        if !self.json {
            let path = self.path();
            if let Err(error) = std::fs::write(&path, format!("{HEADER}\n")) {
                tracing::warn!(%error, "{} was not written", path.display());
            }
        }
    }

    /// Adds the entry of a task: a CSV row at once, or a JSON entry for the end of the run.
    fn add(&mut self, task: &Task) {
        let field = |key: &str| (task.record.get(key)).and_then(serde_json::Value::as_str);
        let name = field("name").unwrap_or_default().to_owned();
        let process = name
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_owned();
        let mut values = usage(&task.stderr);
        if !values.is_empty() {
            values.push(("queue", field("queue").unwrap_or_default().to_owned()));
        }
        let value = |key: &str| (values.iter().find(|(name, _)| *name == key)).map(|(_, v)| v);
        if self.json {
            let mut entry = Json::object(FIELDS.map(|key| {
                (
                    key,
                    value(key).map_or(Json::Null, |text| Json::Text(text.clone())),
                )
            }));
            let queue = value("queue").map_or(Json::Null, |queue| Json::Text(queue.clone()));
            entry.insert("queue", queue);
            entry.insert("name", Json::Text(name));
            match self.report.iter_mut().find(|(key, _)| *key == process) {
                Some((_, entries)) => entries.push(entry),
                None => self.report.push((process, vec![entry])),
            }
            return;
        }
        // A missing value is an empty cell.
        let empty = String::new();
        let mut cells = vec![&name, &process, value("queue").unwrap_or(&empty)];
        cells.extend(COLUMNS.map(|key| value(key).unwrap_or(&empty)));
        let row: Vec<&str> = cells.iter().map(|cell| cell.as_str()).collect();
        let path = self.path();
        let written = (std::fs::OpenOptions::new().append(true).create(true))
            .open(&path)
            .and_then(|mut file| writeln!(file, "{}", row.join(",")));
        if let Err(error) = written {
            tracing::warn!(%error, "{} was not written", path.display());
        }
    }

    /// Writes the JSON report, and copies the report to `gadi.publishTo`.
    fn ended(&mut self) {
        let path = self.path();
        if self.json {
            let report = Json::object(
                (self.report.iter())
                    .map(|(process, entries)| (process.clone(), Json::list(entries.clone()))),
            );
            if let Err(error) = std::fs::write(&path, report.pretty()) {
                tracing::warn!(%error, "{} was not written", path.display());
            }
        }
        if let Some(target) = &self.publish {
            match publish(&path, &self.launch, target) {
                Ok(()) => tracing::info!("Usage report published to {target}"),
                Err(message) => {
                    tracing::warn!("Failed to publish usage report to '{target}': {message}")
                }
            }
        }
    }
}

/// Copies the report to a local path, replacing a file that is there.
fn publish(report: &Path, launch: &Path, target: &str) -> Result<(), String> {
    if target.contains("://") {
        return Err("uwe copies the usage report only to a local path".into());
    }
    let target = launch.join(target);
    if let Some(folder) = target.parent() {
        std::fs::create_dir_all(folder).map_err(|error| error.to_string())?;
    }
    std::fs::copy(report, &target).map_err(|error| error.to_string())?;
    Ok(())
}

/// The fields of the Gadi job summary in a task's standard error, by nf-gadi's keys, with the
/// efficiency; none when the summary is not of a Gadi job.
fn usage(stderr: &str) -> Vec<(&'static str, String)> {
    let gadi = (stderr.lines())
        .filter_map(|line| after(line, "Job Id:"))
        .any(|id| id.contains("gadi-pbs"));
    if !gadi {
        return Vec::new();
    }
    let labels = [
        ("sus", "Service Units:", Form::Decimal),
        ("walltime", "Walltime Used:", Form::Clock),
        ("cputime", "CPU Time Used:", Form::Clock),
        ("cpus", "NCPUs Requested:", Form::Count),
        ("memory", "Memory Used:", Form::Size),
        ("exitCode", "Exit Status:", Form::Signed),
        ("requestedMemory", "Memory Requested:", Form::Size),
        ("requestedWalltime", "Walltime Requested:", Form::Clock),
        ("requestedJobFS", "JobFS Requested:", Form::Size),
        ("usedJobFS", "JobFS Used:", Form::Word),
    ];
    let mut found: Vec<(&'static str, String)> = Vec::new();
    for line in stderr.lines() {
        for (key, label, form) in &labels {
            // Since 1.4.0 the memory of the GPUs is not the memory of the job.
            let value = match *key {
                "memory" => (line.match_indices(label))
                    .filter(|(at, _)| !line[..*at].ends_with("GPU "))
                    .find_map(|(at, _)| after(&line[at..], label)),
                _ => after(line, label),
            };
            if let Some(value) = value.filter(|value| form.fits(value)) {
                found.retain(|(name, _)| name != key);
                found.push((key, value.to_owned()));
            }
        }
    }
    let field = |key: &str| (found.iter().find(|(name, _)| *name == key)).map(|(_, v)| v.clone());
    if let Some(cpu) = field("cputime") {
        let minutes = |text: Option<String>| {
            let parts: Vec<f64> = (text.unwrap_or_default().split(':'))
                .map(|part| part.parse().unwrap_or(0.0))
                .collect();
            match parts.as_slice() {
                [hours, minutes, seconds] => hours * 60.0 + minutes + seconds / 60.0,
                _ => 0.0,
            }
        };
        let cpus: f64 = field("cpus")
            .and_then(|cpus| cpus.parse().ok())
            .unwrap_or(0.0);
        let efficiency = minutes(Some(cpu)) / minutes(field("walltime")) / cpus.ceil();
        found.push(("efficiency", format!("{efficiency:.2}")));
    }
    found
}

/// The first word after `label` in a line.
fn after<'a>(line: &'a str, label: &str) -> Option<&'a str> {
    let (_, rest) = line.split_once(label)?;
    rest.split_whitespace().next()
}

/// How the value of a label of the summary looks.
#[derive(Clone, Copy)]
enum Form {
    /// A decimal with a dot, like `0.01`.
    Decimal,
    /// `HH:MM:SS`.
    Clock,
    Count,
    /// An integer that may be negative.
    Signed,
    /// A decimal and a unit, like `11.5MB`.
    Size,
    Word,
}

impl Form {
    fn fits(self, text: &str) -> bool {
        let decimal = |text: &str| {
            let (whole, part) = text.split_once('.').unwrap_or_default();
            number(whole) && number(part)
        };
        match self {
            Form::Decimal => decimal(text),
            Form::Clock => text.split(':').count() == 3 && text.split(':').all(number),
            Form::Count => number(text),
            Form::Signed => number(text.strip_prefix('-').unwrap_or(text)),
            Form::Size => {
                let size = text.trim_end_matches(|c: char| c.is_ascii_alphabetic());
                size.len() < text.len() && (number(size) || decimal(size))
            }
            Form::Word => !text.is_empty(),
        }
    }
}

fn number(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settles_report_names() {
        assert_eq!(settle(None, None), (false, "UsageReport.csv".into()));
        assert_eq!(
            settle(Some("json"), None),
            (true, "UsageReport.json".into())
        );
        assert_eq!(settle(Some("csv"), Some("r.json")), (true, "r.json".into()));
        assert_eq!(
            settle(Some("csv"), Some("r.txt")),
            (false, "UsageReport.csv".into())
        );
        assert_eq!(settle(None, Some("r.json")), (true, "r.json".into()));
        assert_eq!(settle(None, Some("r")), (false, "UsageReport.csv".into()));
        assert_eq!(settle(Some("xml"), None), (false, "UsageReport.csv".into()));
    }

    #[test]
    fn reads_gadi_summary() {
        let summary = "   Job Id:             140000001.gadi-pbs\n   Exit Status:        0\n   \
                       Service Units:      0.01\n   NCPUs Requested:    2      NCPUs Used: 2\n   \
                       CPU Time Used: 00:00:01\n   Memory Requested:   1.0GB   Memory Used: \
                       11.5MB\n   GPU Memory Used:    99.0GB\n   Walltime Requested: 01:00:00   \
                       Walltime Used: 00:00:02\n   JobFS Requested:    100.0MB   JobFS Used: 0B\n";
        let found = usage(summary);
        let value =
            |key: &str| (found.iter().find(|(name, _)| *name == key)).map(|(_, v)| v.as_str());
        assert_eq!(value("memory"), Some("11.5MB"));
        assert_eq!(value("requestedWalltime"), Some("01:00:00"));
        assert_eq!(value("efficiency"), Some("0.25"));
        assert_eq!(value("usedJobFS"), Some("0B"));
        assert!(usage(&summary.replace("gadi-pbs", "server")).is_empty());
    }
}
