//! The worker thread of nf-tower, which sends the run's requests in their order.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::Method;

use super::Shared;
use super::client::Problem;
use super::limits;
use super::metrics::Metrics;
use super::reports::Reports;
use super::settings::Settings;
use super::trace::{dag, record};
use crate::observer::{End, Event, Start};
use crate::report_format::{Json, now, offset_time};
use crate::run_metadata::Run;
use crate::service::{self, Sender};

/// Task records wait this long for more before they are sent, unless 100 are waiting.
const BATCH: Duration = Duration::from_secs(1);
const HEARTBEAT: Duration = Duration::from_secs(60);

/// What the worker should do on a tick.
#[derive(Debug, PartialEq)]
enum Due {
    Progress,
    Heartbeat,
    Nothing,
}

/// Records go out after `BATCH`, or at once when 100 wait or the run asks; a quiet run sends
/// a heartbeat each `HEARTBEAT`.
fn due(since: Duration, waiting: usize, flush: bool) -> Due {
    match waiting {
        0 if since > HEARTBEAT => Due::Heartbeat,
        0 => Due::Nothing,
        _ if flush || waiting >= 100 || since > BATCH => Due::Progress,
        _ => Due::Nothing,
    }
}

/// Sends the requests in the worker thread.
pub(super) struct Worker {
    settings: Settings,
    sender: Option<Sender>,
    shared: Arc<Shared>,
    run: Option<Run>,
    workflow_id: String,
    watch_url: Option<String>,
    /// The latest record of each task that changed since the last progress request.
    tasks: BTreeMap<i64, Json>,
    last: Instant,
    metrics: Metrics,
    reports: Option<Reports>,
    /// The run began on the Platform.
    begun: bool,
    /// Nothing more is sent: a request failed, or the run was completed.
    stopped: bool,
}

impl Worker {
    pub(super) fn new(settings: Settings, shared: Arc<Shared>) -> Worker {
        Worker {
            settings,
            sender: None,
            shared,
            run: None,
            workflow_id: String::new(),
            watch_url: None,
            tasks: BTreeMap::new(),
            last: Instant::now(),
            metrics: Metrics::default(),
            reports: None,
            begun: false,
            stopped: false,
        }
    }

    pub(super) fn handle(&mut self, event: Option<Event>) {
        match &event {
            Some(Event::RunStarted(start)) => self.begin(start),
            Some(Event::FilePublished { target, .. }) => {
                if let Some(reports) = &mut self.reports {
                    reports.publish(target);
                }
            }
            Some(Event::RunEnded(end)) => self.complete(end),
            Some(event) => self.task(event),
            None => {}
        }
        if event.is_some() {
            self.shared.update(|status| status.heard += 1);
        }
        self.tick();
    }

    /// Creates the run on the Platform, then begins it with the workflow and its processes.
    fn begin(&mut self, start: &Start) {
        let run = Run::new(start);
        let create = Json::object([
            ("sessionId", Json::from(&start.session_id)),
            ("projectName", Json::from(&start.project_name)),
            ("repository", Json::from(start.repository.clone())),
            ("runName", Json::from(&start.run_name)),
            ("workflowId", Json::from(self.settings.launched.clone())),
            ("instant", Json::from(now())),
        ]);
        let created = match self.send(Method::POST, "/trace/create", create) {
            Ok(created) => created,
            Err(problem) => return self.abort(&problem),
        };
        if let Some(message) = created["message"].as_str() {
            tracing::warn!("{message}");
        }
        let Some(id) = created["workflowId"].as_str() else {
            return self.fail("Invalid Seqera Platform API response - Missing workflow Id".into());
        };
        self.workflow_id = id.to_owned();
        self.watch_url = created["watchUrl"].as_str().map(str::to_owned);
        match Reports::load(start, id, self.settings.reports_file.as_deref()) {
            Ok(reports) => self.reports = reports,
            Err(message) => return self.fail(message),
        }
        self.run = Some(run);
        let body = Json::object([
            ("workflow", self.workflow(false)),
            (
                "processNames",
                Json::list(start.processes.iter().map(|p| &p.name)),
            ),
            ("towerLaunch", Json::from(self.settings.launched.is_some())),
            ("instant", Json::from(now())),
            ("dag", dag(start)),
        ]);
        let path = format!("/trace/{}/begin", self.workflow_id);
        let begun = match self.send(Method::PUT, &path, body) {
            Ok(begun) => begun,
            Err(problem) => return self.abort(&problem),
        };
        if self.watch_url.is_none() {
            self.watch_url = begun["watchUrl"].as_str().map(str::to_owned);
        }
        if let Some(url) = &self.watch_url {
            tracing::info!("Monitor the execution with Seqera Platform using this URL: {url}");
        }
        self.begun = true;
        self.last = Instant::now();
        self.shared.update(|status| status.ready = true);
    }

    /// Keeps the latest record of a task for the next progress request.
    fn task(&mut self, event: &Event) {
        let (Event::TaskSubmitted(task)
        | Event::TaskStarted(task)
        | Event::TaskCompleted(task)
        | Event::TaskCached(task)
        | Event::TaskFailed(task)) = event
        else {
            return;
        };
        if let Some(run) = &mut self.run {
            run.stats.count(event);
        }
        if matches!(
            event,
            Event::TaskCompleted(_) | Event::TaskCached(_) | Event::TaskFailed(_)
        ) {
            self.metrics.add(task);
        }
        let id = task.record.get("task_id").and_then(|id| id.as_i64());
        self.tasks.insert(id.unwrap_or_default(), record(task));
    }

    fn tick(&mut self) {
        if !self.begun || self.stopped {
            return;
        }
        let mut flush = false;
        self.shared.update(|status| {
            flush = !status.flushed && status.flush.is_some_and(|sent| status.heard >= sent);
        });
        match due(self.last.elapsed(), self.tasks.len(), flush) {
            Due::Progress => self.progress(),
            Due::Heartbeat => self.heartbeat(),
            Due::Nothing => {}
        }
        if flush {
            self.shared.update(|status| status.flushed = true);
        }
    }

    fn progress(&mut self) {
        let Some(run) = &self.run else {
            return;
        };
        let tasks = std::mem::take(&mut self.tasks);
        let body = Json::object([
            ("tasks", Json::List(tasks.into_values().collect())),
            ("progress", run.stats.progress()),
            ("containers", Json::List(Vec::new())),
            ("instant", Json::from(now())),
        ]);
        let path = format!("/trace/{}/progress", self.workflow_id);
        self.last = Instant::now();
        if let Err(problem) = self.send(Method::PUT, &path, body) {
            self.fail(format!(
                "Aborting session due to Seqera Platform telemetry exception - {}",
                problem.report()
            ));
        }
    }

    fn heartbeat(&mut self) {
        let Some(run) = &self.run else {
            return;
        };
        let body = Json::object([
            ("progress", run.stats.progress()),
            ("instant", Json::from(now())),
        ]);
        let path = format!("/trace/{}/heartbeat", self.workflow_id);
        self.last = Instant::now();
        if let Err(problem) = self.send(Method::PUT, &path, body) {
            tracing::warn!("{}", problem.warning(&self.settings.api.endpoint));
        }
    }

    /// Sends the records that are left, then completes the run with its statistics.
    fn complete(&mut self, end: &End) {
        if let Some(run) = &mut self.run {
            run.end(end);
        }
        if let Some(task) = &end.task {
            let id = task.record.get("task_id").and_then(|id| id.as_i64());
            self.tasks.insert(id.unwrap_or_default(), record(task));
        }
        if !self.begun || self.stopped {
            self.stopped = true;
            return;
        }
        if !self.tasks.is_empty() {
            self.progress();
        }
        let Some(run) = &self.run else {
            return;
        };
        if self.stopped {
            return;
        }
        let body = Json::object([
            ("workflow", self.workflow(true)),
            ("metrics", self.metrics.json()),
            ("progress", run.stats.progress()),
            ("instant", Json::from(now())),
        ]);
        let path = format!("/trace/{}/complete", self.workflow_id);
        if let Err(problem) = self.send(Method::PUT, &path, body) {
            tracing::warn!("{}", problem.warning(&self.settings.api.endpoint));
        }
        self.stopped = true;
    }

    /// The `workflow` of the begin and complete requests; the complete one has the run's
    /// statistics and no `platform`.
    fn workflow(&self, complete: bool) -> Json {
        let Some(run) = &self.run else {
            return Json::Null;
        };
        let Json::Object(mut entries) = run.workflow(|millis| Json::from(offset_time(millis)))
        else {
            return Json::Null;
        };
        let platform = Json::object([
            ("workflowId", Json::from(&self.workflow_id)),
            ("workflowUrl", Json::from(self.watch_url.clone())),
            ("user", Json::Null),
            ("workspace", Json::Null),
            ("computeEnv", Json::Null),
            ("pipeline", Json::Null),
            ("labels", Json::Null),
            ("schedRunId", Json::Null),
        ]);
        for entry in &mut entries {
            if entry.0 == "platform" {
                entry.1 = platform.clone();
            }
        }
        if complete {
            entries.retain(|(key, _)| key != "platform");
            let place = entries.iter().position(|(key, _)| key == "configFiles");
            let place = place.map_or(entries.len(), |place| place + 1);
            entries.insert(place, ("stats".into(), run.stats.workflow(false)));
        }
        let mut text = self.settings.config_text.clone();
        if !text.lines().any(|line| line.starts_with("runName ")) {
            text.push_str(&format!("runName = '{}'\n", run.start.run_name));
        }
        let logs = self.settings.logs.as_ref();
        entries.extend([
            ("params".into(), Json::from(&run.start.params)),
            ("id".into(), Json::from(&self.workflow_id)),
            ("configText".into(), Json::from(text)),
            (
                "operationId".into(),
                Json::from(logs.map(|logs| logs.operation.clone())),
            ),
            (
                "logFile".into(),
                Json::from(logs.and_then(|logs| logs.log_file.clone())),
            ),
            (
                "outFile".into(),
                Json::from(logs.and_then(|logs| logs.out_file.clone())),
            ),
        ]);
        Json::Object(entries)
    }

    /// Sends a request, its text values cut to the Platform's limits.
    fn send(
        &mut self,
        method: Method,
        path: &str,
        mut body: Json,
    ) -> Result<serde_json::Value, Problem> {
        limits::limit(&mut body);
        let sender = service::sender(&mut self.sender).map_err(|message| Problem {
            url: self.settings.api.url(path),
            code: 0,
            message,
        })?;
        self.settings.api.send(sender, method, path, &body)
    }

    /// Ends the reports after a failed create or begin request, as Nextflow aborts the run.
    fn abort(&mut self, problem: &Problem) {
        self.fail(format!(
            "Abort exception produced when notifying an event - {}",
            problem.report()
        ));
    }

    fn fail(&mut self, message: String) {
        tracing::error!("{message}");
        self.stopped = true;
        self.shared.update(|status| {
            status.ready = true;
            status.failure.get_or_insert(message);
        });
    }
}
