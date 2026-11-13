//! nf-co2footprint 1.4.1: the energy use and CO₂ equivalents of a run's tasks by the Green
//! Algorithms method, as a trace, a summary, an HTML report and a JSON-LD provenance file.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod calculation;
mod carbon;
mod files;
mod readable;
mod report;
mod settings;

use std::collections::BTreeMap;
use std::io::Write as _;

use nextflow_config::Config;

use self::calculation::{Metrics, Warnings, aggregate};
use self::carbon::Intensity;
use self::files::Node;
use self::readable::quantity;
use self::settings::Settings;
use crate::catalogue::{Kind, Plugin, State};
use crate::observer::{End, Event, Observer, Record, Start};

/// The functions and the `postRun` command of the plugin are not implemented.
pub(crate) const PLUGIN: Plugin = Plugin {
    functions: &["parseTraceFile", "calculateCO2"],
    supported: Some(&[]),
    scopes: &["co2footprint"],
    observer: Some(activate),
    ..Plugin::new("nf-co2footprint", Kind::Observer, State::Partial)
};
/// Reads the config, and starts the estimate of the run.
fn activate(
    config: &Config,
    launch: &std::path::Path,
    _: &crate::catalogue::Environment,
) -> Result<Option<Box<dyn Observer>>, String> {
    let settings = settings::read(config, launch)?;
    Ok(Some(Box::new(Footprint {
        settings,
        intensity: None,
        trace: None,
        tasks: Vec::new(),
        running: BTreeMap::new(),
        warnings: Warnings::default(),
        start: None,
    })))
}

/// The estimate of a run as its events come.
struct Footprint {
    settings: Settings,
    intensity: Option<Intensity>,
    trace: Option<std::fs::File>,
    tasks: Vec<Metrics>,
    /// The record of each task that started and has not ended, by task id.
    running: BTreeMap<i64, Record>,
    warnings: Warnings,
    start: Option<Box<Start>>,
}

impl Observer for Footprint {
    fn notify(&mut self, event: &Event) {
        match event {
            Event::RunStarted(start) => self.started(start),
            Event::TaskSubmitted(task) | Event::TaskStarted(task) => {
                let id = task.record["task_id"].as_i64().unwrap_or_default();
                self.running.insert(id, task.record.clone());
            }
            Event::TaskCompleted(task) | Event::TaskCached(task) | Event::TaskFailed(task) => {
                let id = task.record["task_id"].as_i64().unwrap_or_default();
                self.running.remove(&id);
                self.add(&task.record);
            }
            Event::RunEnded(end) => self.finish(end),
            Event::FilePublished { .. } => {}
        }
    }
}

impl Footprint {
    fn started(&mut self, start: &Start) {
        let settings = &self.settings;
        self.intensity = Some(match &settings.token {
            Some(token) => {
                let zone = settings.zone.clone().unwrap_or_default();
                Intensity::live(settings.intensity, zone, token.clone())
            }
            None => Intensity::fixed(settings.intensity),
        });
        let trace = &settings.trace;
        if trace.enabled && !(trace.path.exists() && !trace.overwrite) {
            let parent = trace.path.parent().map(std::fs::create_dir_all);
            let opened = (parent.unwrap_or(Ok(())))
                .and_then(|()| std::fs::File::create(&trace.path))
                .and_then(|mut file| file.write_all(files::TRACE.as_bytes()).map(|()| file));
            match opened {
                Ok(file) => self.trace = Some(file),
                Err(error) => tracing::warn!(%error, "[nf-co2footprint] the trace was not written"),
            }
        }
        self.start = Some(Box::new(start.clone()));
    }

    /// Estimates a task that ended and adds its line to the trace.
    fn add(&mut self, record: &Record) {
        let time = |key: &str| record.get(key).and_then(serde_json::Value::as_i64);
        let intensity = match &self.intensity {
            Some(intensity) => intensity.between(time("start"), time("complete")),
            None => self.settings.intensity,
        };
        let Some(task) = self
            .settings
            .model
            .task(record, intensity, &mut self.warnings)
        else {
            return;
        };
        if let Some(trace) = &mut self.trace
            && let Err(error) = trace.write_all(files::trace_row(&task).as_bytes())
        {
            tracing::warn!(%error, "[nf-co2footprint] a trace line was not written");
        }
        self.tasks.push(task);
    }

    /// Estimates the tasks left and the engine itself, then writes the files of the run.
    fn finish(&mut self, end: &End) {
        if let Some(intensity) = &mut self.intensity {
            intensity.stop();
        }
        for record in std::mem::take(&mut self.running).into_values() {
            self.add(&record);
        }
        self.trace.take();
        let Some(start) = self.start.take() else {
            return;
        };
        let run = &start.run_name;
        let mut processes: Vec<(String, Vec<&Metrics>)> = Vec::new();
        for task in &self.tasks {
            match processes.iter_mut().find(|(name, _)| *name == task.process) {
                Some((_, tasks)) => tasks.push(task),
                None => processes.push((task.process.clone(), vec![task])),
            }
        }
        let processes: Vec<Node> = (processes.into_iter())
            .map(|(name, tasks)| {
                let owned: Vec<Metrics> = tasks.into_iter().cloned().collect();
                let children = (owned.iter())
                    .map(|task| Node {
                        level: "task",
                        name: task.task_id.unwrap_or_default().to_string(),
                        metrics: task.clone(),
                        children: Vec::new(),
                    })
                    .collect();
                Node {
                    level: "process",
                    metrics: aggregate(&name, &owned),
                    name,
                    children,
                }
            })
            .collect();
        let sums: Vec<Metrics> = processes.iter().map(|node| node.metrics.clone()).collect();
        let workflow = Node {
            level: "workflow",
            name: run.clone(),
            metrics: aggregate(run, &sums),
            children: processes,
        };
        tracing::info!(
            "[nf-co2footprint] The workflow run used {} of electricity, resulting in the release \
             of {} of CO\u{2082} equivalents into the atmosphere.",
            quantity(workflow.metrics.energy * 1000.0, "Wh"),
            quantity(workflow.metrics.co2, "g"),
        );
        let head = self.head(run, start.start, end.end);
        let parts = [head.metrics.clone(), workflow.metrics.clone()];
        let session_name = format!("{run}-session");
        let session = Node {
            level: "session",
            metrics: aggregate(&session_name, &parts),
            name: session_name,
            children: vec![head, workflow],
        };
        let settings = &self.settings;
        settings
            .summary
            .write(&files::summary(&session.metrics, &settings.options));
        let shown: Vec<&Metrics> = self.tasks.iter().take(settings.max_tasks).collect();
        let header = report::Header {
            run_name: run,
            command_line: &start.command_line,
            project: &start.project_dir,
            start: start.start,
            end: end.end,
        };
        let records = self
            .intensity
            .as_ref()
            .map(Intensity::records)
            .unwrap_or_default();
        let page = report::render(&header, &session, &shown, &settings.options, &records);
        settings.report.write(&page);
        let provenance = files::provenance(&session, settings.metrics_only, settings.nulls);
        settings.provenance.write(&provenance);
    }

    /// The estimate of the engine process itself, from its CPU time and its peak of memory.
    fn head(&mut self, run: &str, start: i64, end: i64) -> Node {
        let realtime = (end - start).max(0);
        let (cpu_millis, peak) = head_usage().unwrap_or_default();
        let cpu = match realtime {
            0 => 0.0,
            _ => cpu_millis / realtime as f64 * 100.0,
        };
        let name = format!("{run}-head_job");
        let record = serde_json::json!({
            "task_id": -1, "status": "COMPLETED", "name": name, "realtime": realtime,
            "cpus": 1, "%cpu": cpu, "memory": peak,
        });
        let record = record.as_object().cloned().unwrap_or_default();
        let intensity = match &self.intensity {
            Some(intensity) => intensity.between(Some(start), Some(end)),
            None => self.settings.intensity,
        };
        let task = self
            .settings
            .model
            .task(&record, intensity, &mut self.warnings);
        let mut metrics = task.unwrap_or_default();
        metrics.task_id = None;
        metrics.record = Record::new();
        Node {
            level: "head",
            name,
            metrics,
            children: Vec::new(),
        }
    }
}

/// The CPU time in milliseconds and the peak of resident memory in bytes of this process, from
/// `/proc/self`; `None` where the system has no such files.
fn head_usage() -> Option<(f64, f64)> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    let fields: Vec<&str> = stat.rsplit_once(')')?.1.split_whitespace().collect();
    let ticks = |index: usize| {
        fields
            .get(index)
            .and_then(|field| field.parse::<f64>().ok())
    };
    // The kernel counts CPU time in ticks of 1/100 second on Linux.
    let cpu = (ticks(11)? + ticks(12)?) * 10.0;
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let peak = (status.lines())
        .find_map(|line| line.strip_prefix("VmHWM:"))
        .and_then(|value| {
            value
                .trim()
                .trim_end_matches("kB")
                .trim()
                .parse::<f64>()
                .ok()
        })
        .map_or(0.0, |kilobytes| kilobytes * 1024.0);
    Some((cpu, peak))
}
