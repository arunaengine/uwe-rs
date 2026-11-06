//! nf-biffix 0.6.2: reports the run and its tasks in biffix's engine event format, posted to the
//! run's ingest route and written to its events file.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod journal;
mod poster;

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::time::Duration;

use nextflow_config::Config;

use crate::catalogue::{Kind, Plugin, State};
use crate::observer::{Background, End, Event, Observer, Start, Task};
use crate::report_format::{Json, now};
use crate::service;
use journal::Journal;
use poster::Poster;

pub(crate) const PLUGIN: Plugin = Plugin {
    observer: Some(activate),
    ..Plugin::new("nf-biffix", Kind::Observer, State::Partial)
};

/// How often the worker looks at its queue and its buffered publications.
const TICK: Duration = Duration::from_millis(250);
/// How long the end of the run waits for the last events.
const WAIT: Duration = Duration::from_secs(60);

/// The variables that a biffix coordinator sets, read once.
struct Settings {
    run_id: String,
    url: String,
    token: String,
    file: String,
    /// The coordinator's attempt, which prefixes execution ids when above 1.
    attempt: u32,
}

fn activate(_: &Config) -> Result<Option<Box<dyn Observer>>, String> {
    let variable = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
    let Some(settings) = Settings::read(&variable) else {
        return Ok(None);
    };
    let mut biffix = Biffix::new(settings);
    let background = Background::ticking("nf-biffix", WAIT, TICK, move |event| {
        biffix.handle(event.as_ref());
    })
    .map_err(|error| error.to_string())?;
    Ok(Some(Box::new(background)))
}

impl Settings {
    /// The settings, `None` unless the run id, ingest address and token are all set.
    fn read(variable: &dyn Fn(&str) -> Option<String>) -> Option<Settings> {
        let names = ["BIFFIX_RUN_ID", "BIFFIX_INGEST_URL", "BIFFIX_INGEST_TOKEN"];
        let values: Vec<Option<String>> = names.iter().map(|name| variable(name)).collect();
        let missing: Vec<&str> = (names.iter().zip(&values))
            .filter(|(_, value)| value.is_none())
            .map(|(name, _)| *name)
            .collect();
        match missing.len() {
            0 => {}
            3 => return None,
            _ => {
                tracing::warn!(
                    "nf-biffix: {} not set; this run is not tracked by biffix",
                    missing.join(", ")
                );
                return None;
            }
        }
        let mut values = values.into_iter().flatten();
        let (run_id, url, token) = (values.next()?, values.next()?, values.next()?);
        Some(Settings {
            run_id,
            url: format!("{}/events", url.trim_end_matches('/')),
            token,
            file: variable("BIFFIX_EVENT_FILE").unwrap_or_else(|| ".biffix_events.jsonl".into()),
            attempt: variable("AWS_BATCH_JOB_ATTEMPT")
                .and_then(|attempt| attempt.trim().parse().ok())
                .unwrap_or(1),
        })
    }
}

/// Turns the run's events into biffix events, in the worker thread.
struct Biffix {
    settings: Settings,
    poster: Poster,
    journal: Option<Journal>,
    /// The key of each task, by task id, fixed at its first use.
    keys: HashMap<i64, String>,
    /// The index of each task in its process, by task id, and the count per process.
    indexes: HashMap<i64, u32>,
    counts: HashMap<String, u32>,
    /// The task key and attempt that made each output path.
    made: HashMap<String, (String, i64)>,
    ended: bool,
}

impl Biffix {
    fn new(settings: Settings) -> Biffix {
        let poster = Poster::new(settings.url.clone(), settings.token.clone());
        Biffix {
            settings,
            poster,
            journal: None,
            keys: HashMap::new(),
            indexes: HashMap::new(),
            counts: HashMap::new(),
            made: HashMap::new(),
            ended: false,
        }
    }

    fn handle(&mut self, event: Option<&Event>) {
        if self.ended {
            return;
        }
        match event {
            Some(Event::RunStarted(start)) => self.started(start),
            Some(Event::TaskSubmitted(task)) => {
                let id = task_id(task);
                if !self.indexes.contains_key(&id) {
                    let count = self.counts.entry(field(task, "process")).or_default();
                    *count += 1;
                    self.indexes.insert(id, *count);
                }
            }
            Some(Event::TaskStarted(task)) => {
                self.created(task);
                let attempt = attempt(task);
                if attempt > 1 {
                    let key = self.key(task);
                    self.emit(
                        "task_retrying",
                        vec![
                            ("task_key", Json::from(key)),
                            ("prev_attempt", Json::from(attempt - 1)),
                            ("next_attempt", Json::from(attempt)),
                        ],
                    );
                }
            }
            Some(Event::TaskCompleted(task)) => self.ended_task(task, "task_succeeded"),
            Some(Event::TaskFailed(task)) => self.ended_task(task, "task_failed"),
            Some(Event::TaskCached(task)) => {
                self.created(task);
                self.ended_task(task, "task_cached");
            }
            Some(Event::FilePublished { .. }) => {}
            Some(Event::RunEnded(end)) => self.finish(end),
            None => {}
        }
        self.poster.tick();
    }

    fn started(&mut self, start: &Start) {
        let path = Path::new(&start.launch_dir).join(&self.settings.file);
        self.journal = Some(Journal::new(path));
        tracing::info!(
            "nf-biffix: tracking run={} -> {}",
            self.settings.run_id,
            service::safe_url(&self.settings.url)
        );
        self.emit(
            "run_started",
            vec![("session_id", Json::from(&start.session_id))],
        );
    }

    /// `task_created` with the tasks whose outputs this task reads.
    fn created(&mut self, task: &Task) {
        let key = self.key(task);
        let mut deps = BTreeSet::new();
        for input in &task.inputs {
            let producer = ancestors(&input.path).find_map(|path| self.made.get(path));
            if let Some((producer, _)) = producer.filter(|(producer, _)| *producer != key) {
                deps.insert(producer.clone());
            }
        }
        let mut body = vec![
            ("task_key", Json::from(key)),
            ("group", Json::from(field(task, "process"))),
        ];
        if !deps.is_empty() {
            body.push(("deps", Json::list(deps)));
        }
        self.emit("task_created", body);
    }

    /// The end of an attempt.
    fn ended_task(&mut self, task: &Task, kind: &str) {
        let key = self.key(task);
        let attempt = attempt(task);
        let id = task_id(task);
        let execution = match self.settings.attempt {
            0 | 1 => id.to_string(),
            coordinator => format!("{coordinator}/{id}"),
        };
        self.emit(
            kind,
            vec![
                ("task_key", Json::from(key.clone())),
                ("attempt", Json::from(attempt)),
                ("execution_id", Json::from(execution)),
            ],
        );
        for output in &task.outputs {
            self.made
                .insert(output.path.clone(), (key.clone(), attempt));
        }
    }

    /// The result of the run, then the last posts.
    fn finish(&mut self, end: &End) {
        self.ended = true;
        let kind = if end.success {
            "run_succeeded"
        } else {
            "run_failed"
        };
        self.emit(kind, Vec::new());
        self.poster.close();
        self.journal = None;
    }

    /// The key of a task: its name, with its index when a tag renamed it.
    fn key(&mut self, task: &Task) -> String {
        let id = task_id(task);
        if let Some(key) = self.keys.get(&id) {
            return key.clone();
        }
        let (name, process) = (field(task, "name"), field(task, "process"));
        let key = match self.indexes.get(&id) {
            Some(index) if name != process && name != format!("{process} ({index})") => {
                format!("{name} #{index}")
            }
            _ => name,
        };
        self.keys.insert(id, key.clone());
        key
    }

    fn emit(&mut self, kind: &str, body: Vec<(&str, Json)>) {
        let mut event = Json::object([
            ("schema_version", Json::from(1i64)),
            ("ts", Json::from(now() as f64 / 1000.0)),
            ("run_id", Json::from(&self.settings.run_id)),
            ("kind", Json::from(kind)),
        ]);
        for (key, value) in body {
            event.insert(key, value);
        }
        let line = event.compact();
        if let Some(journal) = &mut self.journal {
            journal.write(&line);
        }
        self.poster
            .push(serde_json::from_str(&line).unwrap_or_default());
    }
}

fn field(task: &Task, name: &str) -> String {
    match task.record.get(name) {
        Some(serde_json::Value::String(text)) => text.clone(),
        Some(serde_json::Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

fn task_id(task: &Task) -> i64 {
    (task.record.get("task_id"))
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0)
}

fn attempt(task: &Task) -> i64 {
    (task.record.get("attempt"))
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(1)
}

/// A path and the folders above it, nearest first.
fn ancestors(path: &str) -> impl Iterator<Item = &str> {
    std::iter::successors(Some(path), |path| {
        path.rsplit_once('/')
            .map(|(parent, _)| parent)
            .filter(|parent| !parent.is_empty())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needs_all_variables() {
        let none = |_: &str| None;
        assert!(Settings::read(&none).is_none());
        let some = |name: &str| (name != "BIFFIX_INGEST_TOKEN").then(|| "x".to_owned());
        assert!(Settings::read(&some).is_none());
        let all = |name: &str| match name {
            "BIFFIX_INGEST_URL" => Some("http://h/ingest/".to_owned()),
            "AWS_BATCH_JOB_ATTEMPT" => Some("2".to_owned()),
            "BIFFIX_EVENT_FILE" => None,
            _ => Some("x".to_owned()),
        };
        let settings = Settings::read(&all).expect("tracked");
        assert_eq!(
            (settings.url.as_str(), settings.attempt),
            ("http://h/ingest/events", 2)
        );
        assert_eq!(settings.file, ".biffix_events.jsonl");
    }
}
