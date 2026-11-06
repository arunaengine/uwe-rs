//! nf-report 1.1.1: writes the execution, task status and sample status reports of a run as
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod entries;
pub(crate) mod files;
mod settings;

use std::collections::HashMap;
use std::path::PathBuf;

use nextflow_config::Config;
use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::value::Value;

use settings::{Kind as Report, Settings};

use crate::Context;
use crate::catalogue::{Kind, Plugin, State};
use crate::observer::{End, Event, Observer, Start};
use crate::report_format::Json;

pub(crate) const PLUGIN: Plugin = Plugin {
    functions: FUNCTIONS,
    call: Some(call),
    scopes: &["nfreport"],
    observer: Some(activate),
    ..Plugin::new("nf-report", Kind::Observer, State::Partial)
};

pub(crate) const FUNCTIONS: &[&str] = &["getReportConfig", "isReportEnabled", "getReportsDir"];

pub(crate) fn call(
    name: &str,
    arguments: Vec<Value>,
    context: &mut Context,
) -> Result<Value, Error> {
    match name {
        "getReportConfig" => Ok(settings::scope(context.config)),
        "isReportEnabled" => {
            let asked = arguments.first().map(Value::to_string).unwrap_or_default();
            let settings = Settings::of(context.config);
            let enabled = Report::ALL
                .into_iter()
                .any(|kind| kind.key() == asked && settings.report(kind).is_some());
            Ok(Value::Boolean(settings.enabled && enabled))
        }
        "getReportsDir" => {
            let folder = context.config.get(&["nfreport", "outputDir"]);
            let folder = folder.filter(|value| !matches!(value, Value::Null));
            Ok(Value::string(
                folder.map_or("./reports".to_owned(), |value| value.to_string()),
            ))
        }
        _ => Err(Error::Unsupported {
            message: format!("the function `{name}` of the plugin `nf-report`"),
            span: Default::default(),
        }),
    }
}

fn activate(config: &Config) -> Result<Option<Box<dyn Observer>>, String> {
    let settings = Settings::of(config);
    if !settings.enabled {
        tracing::info!("nf-report plugin is disabled");
        return Ok(None);
    }
    tracing::info!(
        "nf-report plugin initialized with {} reports",
        settings.reports.len()
    );
    Ok(Some(Box::new(Reporter {
        settings,
        launch: PathBuf::new(),
        start: None,
        events: Vec::new(),
        tasks: Vec::new(),
        latest: HashMap::new(),
        end: None,
    })))
}

/// What the reports know of the run so far.
struct Reporter {
    settings: Settings,
    launch: PathBuf,
    start: Option<Box<Start>>,
    events: Vec<Json>,
    tasks: Vec<Finished>,
    /// The latest attempt of each task, by its id, as uwe gives retries a new hash.
    latest: HashMap<String, i64>,
    end: Option<End>,
}

impl Observer for Reporter {
    fn notify(&mut self, event: &Event) {
        match event {
            Event::RunStarted(start) => {
                self.launch = PathBuf::from(&start.launch_dir);
                self.start = Some(start.clone());
                self.events.push(entries::event(start, None));
                self.write(Report::Execution);
            }
            Event::TaskSubmitted(task) | Event::TaskStarted(task) => self.note(task),
            Event::TaskCompleted(task) | Event::TaskFailed(task) | Event::TaskCached(task) => {
                self.note(task);
                let cached = matches!(event, Event::TaskCached(_));
                let names = match self.settings.report(Report::Sample) {
                    Some(report) => samples(task, report),
                    None => Vec::new(),
                };
                self.tasks.push(Finished {
                    task: task.clone(),
                    cached,
                    samples: names,
                });
                self.write(Report::Task);
                self.write(Report::Sample);
            }
            Event::RunEnded(end) => {
                self.end = Some(end.clone());
                if let Some(start) = &self.start {
                    self.events.push(entries::event(start, Some(end)));
                }
                for kind in Report::ALL {
                    self.write(kind);
                }
                self.summary();
                if self.settings.email {
                    tracing::warn!(
                        "uwe does not send the nf-report email; the reports are in their folders"
                    );
                }
                tracing::info!("nf-report plugin completed reporting");
            }
            Event::FilePublished { .. } => {}
        }
    }
}

impl Reporter {
    fn note(&mut self, task: &crate::observer::Task) {
        let attempt = task
            .record
            .get("attempt")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(1);
        let key = entries::id(&task.record).unwrap_or_default();
        let latest = self.latest.entry(key).or_insert(attempt);
        *latest = (*latest).max(attempt);
    }

    /// The plugin's status of each finished task, in arrival order.
    fn statuses(&self) -> Vec<&'static str> {
        (self.tasks.iter())
            .map(|task| {
                let key = task.text("task_id").unwrap_or_default();
                let latest = self.latest.get(&key).copied().unwrap_or(1);
                status(task, latest, self.end.as_ref())
            })
            .collect()
    }

    fn path(&self, report: &settings::Report, format: &str) -> PathBuf {
        let name = format!("{}{}{suffix}.{format}", report.prefix, report.name);
        self.launch.join(&report.folder).join(name)
    }

    fn write(&mut self, kind: Report) {
        let Some(report) = self.settings.report(kind).cloned() else {
            return;
        };
        let data = match kind {
            Report::Execution => self.execution(&report),
            Report::Task => self.task_report(&report),
            Report::Sample => self.sample_report(&report),
        };
        for format in &report.formats {
            let path = self.path(&report, format);
            let text = match format.as_str() {
                "json" => Some(data.pretty()),
                "tsv" => Some(self.tsv(kind, &data, &report)),
                "html" => self.html(&data, &report),
                other => {
                    tracing::warn!("nf-report has no format {other}");
                    None
                }
            };
            let Some(text) = text else {
                continue;
            };
            files::write(&path, &text);
            if report.link {
                let folder = path.parent().map(PathBuf::from).unwrap_or_default();
                let link = folder.join(format!("{}-latest.{format}", report.name));
                files::link(&link, &path);
            }
        }
    }

    fn execution(&self, report: &settings::Report) -> Json {
        let events = (self.events.iter()).map(|event| filtered(event.clone(), &report.fields));
        Json::object([
            ("report_type", Json::from(Report::Execution.name())),
            ("generated_at", Json::from(files::now())),
            ("events", Json::List(events.collect())),
        ])
    }

    fn task_report(&self, report: &settings::Report) -> Json {
        let statuses = self.statuses();
        let mut groups: Vec<(&str, Vec<Json>)> = STATUSES
            .iter()
            .map(|status| (*status, Vec::new()))
            .collect();
        for (task, status) in self.tasks.iter().zip(&statuses) {
            // The plugin filters the fields of finished tasks but not of cached ones.
            let entry = if task.cached {
                entry
            } else {
                filtered(entry, &report.fields)
            };
            if let Some((_, group)) = groups.iter_mut().find(|(known, _)| known == status) {
                group.push(entry);
            }
        }
        let count = |status: &str| statuses.iter().filter(|known| **known == status).count() as i64;
            ("total_tasks", Json::from(statuses.len() as i64)),
            ("completed", Json::from(count("COMPLETED"))),
            ("cached", Json::from(count("CACHED"))),
            ("failed", Json::from(count("FAILED"))),
            ("aborted", Json::from(count("ABORTED"))),
            ("retried", Json::from(count("RETRIED"))),
            ("failure_ignored", Json::from(count("IGNORED"))),
        ]);
        let mut data = Json::object([
            ("report_type", Json::from(Report::Task.name())),
            ("generated_at", Json::from(files::now())),
        ]);
        data.insert(
            "tasks_by_status",
            Json::Object(
                groups
                    .into_iter()
                    .map(|(key, value)| (key.to_owned(), Json::List(value)))
                    .collect(),
            ),
        );
        data
    }

    /// The samples of the tasks in first-seen order, each with its tasks and their statuses.
    fn samples(&self) -> Vec<(String, Vec<(&Finished, &'static str)>)> {
        let statuses = self.statuses();
        let mut found: Vec<(String, Vec<(&Finished, &'static str)>)> = Vec::new();
        for (task, status) in self.tasks.iter().zip(statuses) {
            for name in &task.samples {
                match found.iter_mut().find(|(known, _)| known == name) {
                    Some((_, tasks)) => tasks.push((task, status)),
                    None => found.push((name.clone(), vec![(task, status)])),
                }
            }
        }
        found
    }

    fn sample_report(&self, report: &settings::Report) -> Json {
        let samples: Vec<Json> = (self.samples().iter())
            .map(|(name, tasks)| sample(name, tasks, &report.fields))
            .collect();
        let status = |sample: &Json| {
            sample
                .get("status")
                .and_then(Json::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let groups = ["COMPLETED", "PENDING", "PARTIALLY_COMPLETED", "FAILED"];
        let count = |group: &str| {
            samples
                .iter()
                .filter(|sample| status(sample) == group)
                .count() as i64
        };
        let by_status = groups.map(|group| {
            let members = samples
                .iter()
                .filter(|sample| status(sample) == group)
                .cloned();
            (group.to_owned(), Json::List(members.collect()))
        });
        Json::object([
            ("report_type", Json::from(Report::Sample.name())),
            ("generated_at", Json::from(files::now())),
            (
                "summary",
                Json::object([
                    ("total_samples", Json::from(samples.len() as i64)),
                    ("completed", Json::from(count("COMPLETED"))),
                    ("pending", Json::from(count("PENDING"))),
                    (
                        "partially_completed",
                        Json::from(count("PARTIALLY_COMPLETED")),
                    ),
                    ("failed", Json::from(count("FAILED"))),
                ]),
            ),
            ("samples_by_status", Json::Object(by_status.into())),
        ])
    }

    fn tsv(&self, kind: Report, data: &Json, report: &settings::Report) -> String {
        match kind {
            Report::Execution => {
                let events = match data.get("events") {
                    Some(Json::List(events)) => events.clone(),
                    _ => Vec::new(),
                };
                let header = files::columns(events.iter(), &report.fields);
                let rows = (events.iter())
                    .map(|event| {
                        (header.iter())
                            .map(|key| event_cell(key, event.get(key).unwrap_or(&Json::Null)))
                            .collect()
                    })
                    .collect::<Vec<Vec<String>>>();
                files::tsv(&header, &rows)
            }
            Report::Task => {
                let entries: Vec<Json> = (self.tasks.iter())
                    .map(|task| {
                        if task.cached {
                            entry
                        } else {
                            filtered(entry, &report.fields)
                        }
                    })
                    .collect();
                let header = files::columns(entries.iter(), &report.fields);
                let rows = (entries.iter())
                    .map(|entry| {
                        (header.iter())
                            .map(|key| files::cell(entry.get(key).unwrap_or(&Json::Null)))
                            .collect()
                    })
                    .collect::<Vec<Vec<String>>>();
                files::tsv(&header, &rows)
            }
            Report::Sample => {
                let header: Vec<String> = [
                    "sample_id",
                    "status",
                    "total_tasks",
                    "completed_tasks",
                    "cached_tasks",
                    "failed_tasks",
                    "aborted_tasks",
                    "retried_failures",
                    "ignored_failures",
                ]
                .map(str::to_owned)
                .into();
                let rows = (self.samples().iter())
                    .map(|(name, tasks)| {
                        let sample = sample(name, tasks, &[]);
                        let counts = sample.get("task_counts").cloned().unwrap_or(Json::Null);
                        let number =
                            |key: &str| counts.get(key).map(files::groovy).unwrap_or_default();
                        vec![
                            name.clone(),
                            sample.get("status").map(files::groovy).unwrap_or_default(),
                            number("total"),
                            number("completed"),
                            number("cached"),
                            number("failed"),
                            number("aborted"),
                            number("failure_retried"),
                            number("failure_ignored"),
                        ]
                    })
                    .collect::<Vec<Vec<String>>>();
                files::tsv(&header, &rows)
            }
        }
    }

    /// uwe's own HTML of the report, or the user's template.
    fn html(&self, data: &Json, report: &settings::Report) -> Option<String> {
        let Some(template) = &report.template else {
            let title = match report.kind {
                Report::Execution => "Execution report",
                Report::Task => "Task status report",
                Report::Sample => "Sample status report",
            };
            return Some(files::html(title, data));
        };
        let path = self.launch.join(template);
        match std::fs::read_to_string(&path) {
            Ok(text) => files::template(&text, data, &path.to_string_lossy()),
            Err(_) => {
                tracing::warn!("HTML template file not found at: {}", path.display());
                None
            }
        }
    }

    /// The completion summary of the sample status report, when it asks for one.
    fn summary(&self) {
        let Some(report) = self
            .settings
            .report(Report::Sample)
            .filter(|report| report.summary)
        else {
            return;
        };
        let data = self.sample_report(report);
        let mut text = String::from("\n=== Sample Status Summary ===\n");
        let mut troubled = false;
        for (group, title) in [
            ("COMPLETED", "Completed"),
            ("PARTIALLY_COMPLETED", "Partially Completed"),
            ("FAILED", "Failed"),
            ("PENDING", "Pending"),
        ] {
            let members = match data
                .get("samples_by_status")
                .and_then(|groups| groups.get(group))
            {
                Some(Json::List(members)) => members.clone(),
                _ => Vec::new(),
            };
            if members.is_empty() {
                continue;
            }
            troubled |= group == "FAILED" || group == "PARTIALLY_COMPLETED";
            let mut names: Vec<String> = (members.iter())
                .filter_map(|sample| {
                    sample
                        .get("sample_id")
                        .and_then(Json::as_str)
                        .map(str::to_owned)
                })
                .collect();
            names.sort();
            text.push_str(&format!("\n{title} ({}):\n", names.len()));
            for name in names {
                text.push_str(&format!("  - {name}\n"));
            }
        }
        let paths: Vec<String> = (report.formats.iter())
            .map(|format| self.path(report, format).to_string_lossy().into_owned())
            .collect();
        text.push_str(&format!(
            "\nReport: {}\n=============================",
            paths.join(", ")
        ));
        match troubled {
            true => tracing::warn!("{text}"),
            false => tracing::info!("{text}"),
        }
    }
}
