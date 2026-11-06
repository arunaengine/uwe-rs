//! The worker of nf-lamin's run tracking: makes the transform and the run, and ends the run
//! record.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use jiff::Timestamp;
use serde_json::{Value, json};

use super::Shared;
use super::client::Client;
use super::settings::{Settings, merge};
use crate::observer::{End, Event, Start};
use crate::report_format::offset_time;

/// The status codes of a run record.
const SCHEDULED: i64 = -3;
const STARTED: i64 = -1;
const COMPLETED: i64 = 0;
const ERRORED: i64 = 1;

pub(super) struct Tracker {
    settings: Settings,
    client: Client,
    shared: Arc<Shared>,
    /// The records of the run, once made.
    run: Option<Records>,
    /// The ids of named and uid references, by model and reference.
    resolved: HashMap<(String, String), Option<i64>>,
    space: Option<i64>,
    branch: Option<i64>,
    ended: bool,
}

#[derive(Clone)]
struct Records {
    run_uid: String,
}

impl Tracker {
    pub(super) fn new(settings: Settings, client: Client, shared: Arc<Shared>) -> Tracker {
        Tracker {
            settings,
            client,
            shared,
            run: None,
            resolved: HashMap::new(),
            space: None,
            branch: None,
            ended: false,
        }
    }

    pub(super) fn handle(&mut self, event: &Event) {
        match event {
            Event::RunStarted(start) => self.started(start),
            Event::RunEnded(end) => self.finished(end),
            Event::TaskSubmitted(_)
            | Event::TaskStarted(_)
            | Event::TaskCompleted(_)
            | Event::TaskCached(_)
            | Event::TaskFailed(_)
            | Event::FilePublished { .. } => {}
        }
    }

    /// Makes the transform and the run, and marks the run as started.
    fn started(&mut self, start: &Start) {
        let records = match self.create(start) {
            Ok(records) => records,
            Err(error) => {
                let failure = format!("Could not initialize Lamin run: {error}");
                tracing::error!("{failure}");
                return self.shared.update(|status| {
                    status.ready = true;
                    status.failure = Some(failure);
                });
            }
        };
        self.shared.update(|status| status.ready = true);
        let body = json!({"started_at": offset_time(start.start), "_status_code": STARTED});
        match self.client.update("run", &records.run_uid, &body) {
            Ok(()) => tracing::info!("Run {} started", records.run_uid),
            Err(error) => tracing::warn!("nf-lamin: could not mark the run as started: {error}"),
        }
        self.run = Some(records);
    }

    fn create(&mut self, start: &Start) -> Result<Records, String> {
        let (space, branch) = (self.settings.space.clone(), self.settings.branch.clone());
        self.space = self.reference("space", space.as_deref())?;
        self.branch = self.reference("branch", branch.as_deref())?;
        let (transform_id, transform_uid) = self.transform(start)?;
        let (run_id, run_uid) = self.run_record(start, transform_id, &transform_uid)?;
        let projects = self.ids("project", &self.settings.projects.clone())?;
        let mut labels = self.settings.labels.clone();
        merge(&mut labels, &self.settings.run_labels);
        let labels = self.ids("ulabel", &labels)?;
        let row = |key: &str, id: &i64| json!({"run_id": run_id, key: id});
        let rows = projects.iter().map(|id| row("project_id", id)).collect();
        self.client.upsert("runproject", rows)?;
        let rows = labels.iter().map(|id| row("ulabel_id", id)).collect();
        self.client.upsert("runulabel", rows)?;
        Ok(Records { run_uid })
    }

    /// The transform of the pipeline and version: the configured one, an existing one, or a
    /// new one with its links.
    fn transform(&mut self, start: &Start) -> Result<(i64, String), String> {
        if let Some(uid) = self.settings.transform_uid.clone() {
            let found = self.client.record("transform", &uid)?;
            let record = found.ok_or_else(|| format!("Could not find transform '{uid}'"))?;
            return Ok((id(&record)?, uid));
        }
        let reference = start
            .repository
            .clone()
            .unwrap_or_else(|| start.project_name.clone());
        let script = Path::new(&start.script);
        let script = (script.strip_prefix(&start.project_dir).unwrap_or(script))
            .to_string_lossy()
            .into_owned();
        let key = match script.as_str() {
            "main.nf" => reference.clone(),
            _ => format!("{reference}:{script}"),
        };
        let version = (start.revision.clone())
            .or_else(|| start.commit_id.clone())
            .unwrap_or_else(|| "local-development".to_owned());
        let filter = json!({"and": [{"key": {"eq": key}}, {"version_tag": {"eq": version}}]});
        let found = self.client.find("transform", filter, true)?;
        if found.len() > 1 {
            tracing::warn!(
                "Found multiple Transform objects with key {key} and version_tag {version}"
            );
        }
        if let Some(record) = found.first() {
            let uid = text(&record["uid"]);
            tracing::info!("Using existing transform {uid}");
            return Ok((id(record)?, uid));
        }
        let mut source = format!("repo: {reference}\npath: {script}");
        match (&start.commit_id, &start.revision) {
            (Some(commit), _) => source.push_str(&format!("\ncommit: {commit}")),
            (None, Some(revision)) => source.push_str(&format!("\nbranch: {revision}")),
            (None, None) => {}
        }
        let manifest = |field: &str, missing: &str| {
            (start.manifest[field].as_str())
                .filter(|text| !text.is_empty())
                .unwrap_or(missing)
                .to_owned()
        };
        let description = format!(
            "{}: {}",
            manifest("name", "<No name in manifest>"),
            manifest("description", "<No description in manifest>")
        );
        let body = json!({
            "key": key,
            "kind": "pipeline",
            "source_code": source,
            "kwargs": {
                "reference": reference,
                "version_tag": version,
                "description": description,
                "reference_type": "url",
            },
        });
        let record = self.client.transform(&body)?;
        let (transform_id, uid) = (id(&record)?, text(&record["uid"]));
        if let Some(placement) = self.placement() {
            self.client.update("transform", &uid, &placement)?;
        }
        let (owner, name) = (&self.settings.hub.owner, &self.settings.hub.name);
        let web = self.settings.web.trim_end_matches('/');
        tracing::info!("Created new transform {uid} ({web}/{owner}/{name}/transform/{uid})");
        let projects = self.ids("project", &self.settings.projects.clone())?;
        let mut labels = self.settings.labels.clone();
        merge(&mut labels, &self.settings.transform_labels);
        let labels = self.ids("ulabel", &labels)?;
        let row = |key: &str, id: &i64| json!({"transform_id": transform_id, key: id});
        let rows = projects.iter().map(|id| row("project_id", id)).collect();
        self.client.upsert("transformproject", rows)?;
        let rows = labels.iter().map(|id| row("ulabel_id", id)).collect();
        self.client.upsert("transformulabel", rows)?;
        Ok((transform_id, uid))
    }

    /// The run record: the scheduled run of `run_uid` when it belongs to the transform, else
    /// a new one.
    fn run_record(
        &mut self,
        start: &Start,
        transform: i64,
        uid: &str,
    ) -> Result<(i64, String), String> {
        if let Some(wanted) = self.settings.run_uid.clone() {
            match self.client.record("run", &wanted)? {
                Some(record)
                    if record["transform_id"].as_i64() == Some(transform)
                        && record["_status_code"].as_i64() == Some(SCHEDULED) =>
                {
                    return Ok((id(&record)?, wanted));
                }
                _ => tracing::warn!(
                    "nf-lamin: the run {wanted} is not a scheduled run of this transform; \
                     a new run is made"
                ),
            }
        }
        let time = utc_time(start.start);
        let mut body = json!({
            "transform_id": transform,
            "name": start.run_name,
            "created_at": time,
            "started_at": time,
            "_status_code": SCHEDULED,
        });
        if let (Some(placement), Some(body)) = (self.placement(), body.as_object_mut()) {
            body.extend(placement.as_object().cloned().unwrap_or_default());
        }
        let record = self.client.create("run", &body)?;
        let run_uid = text(&record["uid"]);
        let (owner, name) = (&self.settings.hub.owner, &self.settings.hub.name);
        let web = self.settings.web.trim_end_matches('/');
        tracing::info!("Created new run ({web}/{owner}/{name}/transform/{uid}/{run_uid})");
        Ok((id(&record)?, run_uid))
    }

    /// The space and branch fields of new records, when the config names them.
    fn placement(&self) -> Option<Value> {
        let mut fields = serde_json::Map::new();
        if let Some(space) = self.space {
            fields.insert("space_id".into(), space.into());
        }
        if let Some(branch) = self.branch {
            fields.insert("branch_id".into(), branch.into());
            fields.insert("created_on_id".into(), branch.into());
        }
        (!fields.is_empty()).then_some(Value::Object(fields))
    }

    /// Ends the run record, once.
    fn finished(&mut self, end: &End) {
        if std::mem::replace(&mut self.ended, true) {
            return;
        }
        let Some(run) = self.run.clone() else {
            return;
        };
        let code = if end.success { COMPLETED } else { ERRORED };
        let body = json!({"finished_at": offset_time(end.end), "_status_code": code});
        let state = if end.success { "completed" } else { "errored" };
        match self.client.update("run", &run.run_uid, &body) {
            Ok(()) => tracing::info!("Run '{}' {state}", run.run_uid),
            Err(error) => tracing::warn!("nf-lamin: could not end the run record: {error}"),
        }
    }

    /// The ids of references, skipping those that are optional or not found.
    fn ids(&mut self, model: &str, references: &[String]) -> Result<Vec<i64>, String> {
        let mut ids = Vec::new();
        for reference in references {
            if let Some(id) = self.reference(model, Some(reference))? {
                ids.push(id);
            }
        }
        Ok(ids)
    }

    /// The id of a record by uid, or by name: `?name` may be missing, `!name` must exist and
    /// `+name` is made when missing.
    fn reference(&mut self, model: &str, reference: Option<&str>) -> Result<Option<i64>, String> {
        let Some(reference) = reference.filter(|reference| !reference.is_empty()) else {
            return Ok(None);
        };
        let key = (model.to_owned(), reference.to_owned());
        if let Some(known) = self.resolved.get(&key) {
            return Ok(*known);
        }
        let (mode, name) = match reference.strip_prefix(['?', '!', '+']) {
            Some(name) => (&reference[..1], name),
            None => ("", reference),
        };
        let id = if mode.is_empty() {
            let found = self.client.record(model, name)?;
            if found.is_none() {
                tracing::warn!("Could not find {model} '{name}'");
            }
            found.and_then(|record| record["id"].as_i64())
        } else {
            let filter = json!({"name": {"eq": name}});
            let found = self.client.find(model, filter, false)?;
            match (found.first(), mode) {
                (Some(record), _) => record["id"].as_i64(),
                (None, "?") => {
                    tracing::warn!("Optional core.{model} with name '{name}' not found – skipping");
                    None
                }
                (None, "!") => {
                    return Err(format!(
                        "core.{model} with name '{name}' not found. Use '+{name}' to create it \
                         automatically, or '?{name}' to skip if missing."
                    ));
                }
                (None, _) => self.client.create(model, &json!({"name": name}))?["id"].as_i64(),
            }
        };
        self.resolved.insert(key, id);
        Ok(id)
    }
}

/// A time in UTC with microseconds, like `2026-11-06T12:10:05.921000+00:00`.
fn utc_time(millis: i64) -> String {
    match Timestamp::from_millisecond(millis) {
        Ok(time) => format!(
            "{}.{:06}+00:00",
            time.strftime("%Y-%m-%dT%H:%M:%S"),
            millis.rem_euclid(1000) * 1000
        ),
        Err(_) => String::new(),
    }
}

fn id(record: &Value) -> Result<i64, String> {
    record["id"]
        .as_i64()
        .ok_or_else(|| "a record without an id".to_owned())
}

fn text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_plugin_texts() {
        assert_eq!(
            utc_time(1_793_967_005_921),
            "2026-11-06T12:10:05.921000+00:00"
        );
    }
}
