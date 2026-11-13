//! The worker of nf-lamin's run tracking: makes the transform and the run, registers the
//! remote files that the run reads and writes as artifacts, and ends the run record.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use jiff::Timestamp;
use serde_json::{Value, json};

use super::client::Client;
use super::settings::{Choice, Direction, Settings, merge};
use super::{Annotation, Shared};
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
    /// Folders whose files are not tracked: the work and assets directories, as configured.
    work_dir: String,
    assets_dir: String,
    /// The artifacts made, by storage path.
    artifacts: HashMap<String, Artifact>,
    /// The source of each published file, by target.
    sources: HashMap<String, String>,
    /// The ids of named and uid references, by model and reference.
    resolved: HashMap<(String, String), Option<i64>>,
    space: Option<i64>,
    branch: Option<i64>,
    warned_local: bool,
    ended: bool,
}

#[derive(Clone)]
struct Records {
    transform_uid: String,
    run_id: i64,
    run_uid: String,
}

#[derive(Clone)]
struct Artifact {
    id: i64,
    uid: String,
}

/// What an artifact is made for.
struct Registration {
    /// `None` for the run's report, which has no run and no links.
    direction: Option<Direction>,
    choice: Choice,
    description: String,
}

impl Tracker {
    pub(super) fn new(settings: Settings, client: Client, shared: Arc<Shared>) -> Tracker {
        let home = std::env::var("HOME").unwrap_or_default();
        let assets_dir =
            std::env::var("NXF_ASSETS").unwrap_or_else(|_| format!("{home}/.nextflow/assets"));
        Tracker {
            settings,
            client,
            shared,
            run: None,
            work_dir: String::new(),
            assets_dir,
            artifacts: HashMap::new(),
            sources: HashMap::new(),
            resolved: HashMap::new(),
            space: None,
            branch: None,
            warned_local: false,
            ended: false,
        }
    }

    pub(super) fn handle(&mut self, event: &Event) {
        match event {
            Event::RunStarted(start) => self.started(start),
            Event::TaskCompleted(task) => {
                for input in &task.inputs {
                    self.track(&input.path, Direction::Input, None);
                }
            }
            Event::FilePublished { source, target } => {
                self.sources.insert(target.clone(), source.clone());
                self.track(target, Direction::Output, None);
            }
            Event::RunEnded(end) => self.finished(end),
            Event::TaskSubmitted(_)
            | Event::TaskStarted(_)
            | Event::TaskCached(_)
            | Event::TaskFailed(_) => {}
        }
    }

    /// Makes the transform and the run, marks the run as started and registers the inputs
    /// that the config lists.
    fn started(&mut self, start: &Start) {
        self.work_dir = start.work_dir.clone();
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
        self.shared.update(|status| {
            status.ready = true;
            status.run_uid = Some(records.run_uid.clone());
            status.transform_uid = Some(records.transform_uid.clone());
        });
        let body = json!({"started_at": offset_time(start.start), "_status_code": STARTED});
        match self.client.update("run", &records.run_uid, &body) {
            Ok(()) => tracing::info!("Run {} started", records.run_uid),
            Err(error) => tracing::warn!("nf-lamin: could not mark the run as started: {error}"),
        }
        self.run = Some(records);
        for listed in self.settings.inputs.paths.clone() {
            let choice = Choice {
                kind: listed.kind,
                labels: listed.labels,
            };
            self.listed(&listed.path, Direction::Input, choice, None);
        }
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
        Ok(Records {
            transform_uid,
            run_id,
            run_uid,
        })
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

    /// Registers a file that the run read or wrote, when the rules track it.
    fn track(&mut self, path: &str, direction: Direction, description: Option<String>) {
        if self.run.is_none() || self.artifacts.contains_key(path) || self.local(path) {
            return;
        }
        let tracking = match direction {
            Direction::Input => &self.settings.inputs,
            Direction::Output => &self.settings.outputs,
        };
        let skipped: Vec<&str> = [
            (tracking.skip_work, self.work_dir.as_str()),
            (tracking.skip_assets, self.assets_dir.as_str()),
        ]
        .into_iter()
        .filter_map(|(skip, folder)| skip.then_some(folder))
        .collect();
        if let Some(choice) = tracking.decide(path, &skipped) {
            self.register_file(path, direction, choice, description);
        }
    }

    /// Registers a path of `include_paths`, which the rules do not filter.
    fn listed(&mut self, path: &str, direction: Direction, choice: Choice, note: Option<String>) {
        if !self.artifacts.contains_key(path) && !self.local(path) {
            self.register_file(path, direction, choice, note);
        }
    }

    fn register_file(
        &mut self,
        path: &str,
        direction: Direction,
        choice: Choice,
        note: Option<String>,
    ) {
        let Some(run) = &self.run else {
            return;
        };
        let description = note.unwrap_or_else(|| match direction {
            Direction::Input => format!("Input artifact at {}", nextflow_uri(path)),
            Direction::Output => format!("Output artifact for run {}", run.run_id),
        });
        let registration = Registration {
            direction: Some(direction),
            choice,
            description,
        };
        self.register(path, registration);
    }

    /// Whether a path is on this machine; LaminDB tracks only remote paths, with one warning.
    fn local(&mut self, path: &str) -> bool {
        let local = match path.split_once("://") {
            Some((scheme, _)) => scheme == "file",
            None => true,
        };
        if local && !self.warned_local {
            self.warned_local = true;
            tracing::warn!(
                "Local file detected at {path}. nf-lamin only tracks remote paths (s3://, gs://, \
                 etc.). Local files will be ignored. This warning is only shown once per session."
            );
        }
        local
    }

    /// Makes the artifact of a path and links it to the run, the projects and the labels; a
    /// failure is logged and the run goes on.
    fn register(&mut self, path: &str, registration: Registration) -> Option<Artifact> {
        let run = self.run.clone()?;
        let mut kwargs = serde_json::Map::new();
        if registration.direction == Some(Direction::Output) {
            kwargs.insert("run_id".into(), run.run_id.into());
        }
        kwargs.insert("description".into(), registration.description.into());
        if let Some(kind) = &registration.choice.kind {
            kwargs.insert("kind".into(), kind.clone().into());
        }
        if let Some(placement) = self
            .placement()
            .and_then(|value| value.as_object().cloned())
        {
            kwargs.extend(
                placement
                    .into_iter()
                    .filter(|(key, _)| key != "created_on_id"),
            );
        }
        let body = json!({"path": path, "kwargs": kwargs});
        let record = match self.client.artifact(&body) {
            Ok(record) => record,
            Err(error) => {
                tracing::warn!("nf-lamin: could not register the artifact {path}: {error}");
                return None;
            }
        };
        let artifact = Artifact {
            id: record["id"].as_i64()?,
            uid: text(&record["uid"]),
        };
        let earlier = (record["run_id"].as_i64()).is_some_and(|owner| owner != run.run_id);
        let verb = if earlier {
            "Detected previous"
        } else {
            "Created"
        };
        tracing::info!("{verb} artifact {} for {path}", artifact.uid);
        self.artifacts.insert(path.to_owned(), artifact.clone());
        let Some(direction) = registration.direction else {
            return Some(artifact);
        };
        if direction == Direction::Input {
            let row = json!({"artifact_id": artifact.id, "run_id": run.run_id});
            self.link("artifact_input_of_runs", vec![row], "the input to the run");
        }
        let mut labels = self.settings.labels.clone();
        merge(&mut labels, &registration.choice.labels);
        let projects = self.settings.projects.clone();
        self.annotate(&artifact, &projects, &labels);
        Some(artifact)
    }

    /// Links an artifact to projects and labels.
    fn annotate(&mut self, artifact: &Artifact, projects: &[String], labels: &[String]) {
        for (model, link, key, references) in [
            ("project", "artifactproject", "project_id", projects),
            ("ulabel", "artifactulabel", "ulabel_id", labels),
        ] {
            let ids = match self.ids(model, references) {
                Ok(ids) => ids,
                Err(error) => {
                    tracing::warn!("Could not link {model}: {error}");
                    continue;
                }
            };
            let rows = (ids.iter())
                .map(|id| json!({"artifact_id": artifact.id, key: id, "feature_id": null}))
                .collect();
            self.link(link, rows, model);
        }
    }

    fn link(&mut self, model: &str, rows: Vec<Value>, what: &str) {
        if let Err(error) = self.client.upsert(model, rows) {
            tracing::warn!("Could not link {what}: {error}");
        }
    }

    /// Registers the outputs, applies the annotations, and ends the run record, once.
    fn finished(&mut self, end: &End) {
        if std::mem::replace(&mut self.ended, true) {
            return;
        }
        let Some(run) = self.run.clone() else {
            return;
        };
        for output in &end.outputs {
            let note = format!("Output artifact '{}' for run {}", output.name, run.run_id);
            for file in output.files.iter().chain(&output.index) {
                self.track(file, Direction::Output, Some(note.clone()));
            }
        }
        for listed in self.settings.outputs.paths.clone() {
            let choice = Choice {
                kind: listed.kind,
                labels: listed.labels,
            };
            self.listed(&listed.path, Direction::Output, choice, None);
        }
        self.apply_annotations();
        let report = self.report(&run);
        let code = if end.success { COMPLETED } else { ERRORED };
        let mut body = json!({"finished_at": offset_time(end.end), "_status_code": code});
        if let (Some(report), Some(fields)) = (report, body.as_object_mut()) {
            fields.insert("report_id".into(), report.id.into());
        }
        let state = if end.success { "completed" } else { "errored" };
        match self.client.update("run", &run.run_uid, &body) {
            Ok(()) => tracing::info!("Run '{}' {state}", run.run_uid),
            Err(error) => tracing::warn!("nf-lamin: could not end the run record: {error}"),
        }
    }

    /// Applies the annotations of the script to the artifacts of their files, or of the
    /// published copies of them; the others are named in one warning.
    fn apply_annotations(&mut self) {
        let annotations = match self.shared.status.lock() {
            Ok(mut status) => std::mem::take(&mut status.annotations),
            Err(_) => return,
        };
        let mut merged: Vec<Annotation> = Vec::new();
        for annotation in annotations {
            match merged
                .iter_mut()
                .find(|known| known.path == annotation.path)
            {
                Some(known) => {
                    known.kind = annotation.kind.or(known.kind.take());
                    known.description = annotation.description.or(known.description.take());
                    merge(&mut known.labels, &annotation.labels);
                    merge(&mut known.projects, &annotation.projects);
                }
                None => merged.push(annotation),
            }
        }
        let mut unmatched = Vec::new();
        for annotation in merged {
            let published = (self.sources.iter())
                .find(|(_, source)| **source == annotation.path)
                .map(|(target, _)| target.clone());
            let found = (self.artifacts.get(&annotation.path).cloned())
                .or_else(|| published.and_then(|target| self.artifacts.get(&target).cloned()));
            let Some(artifact) = found else {
                unmatched.push(annotation.path);
                continue;
            };
            let mut fields = serde_json::Map::new();
            if let Some(kind) = annotation.kind {
                fields.insert("kind".into(), kind.into());
            }
            if let Some(description) = annotation.description {
                fields.insert("description".into(), description.into());
            }
            if !fields.is_empty()
                && let Err(error) =
                    self.client
                        .update("artifact", &artifact.uid, &Value::Object(fields))
            {
                tracing::warn!("nf-lamin: could not annotate {}: {error}", artifact.uid);
            }
            self.annotate(&artifact, &annotation.projects, &annotation.labels);
        }
        if !unmatched.is_empty() {
            tracing::warn!(
                "nf-lamin: no artifact was registered for these annotated files: {}",
                unmatched.join(", ")
            );
        }
    }

    /// The execution report as an artifact of the run, when it is a remote file.
    fn report(&mut self, run: &Records) -> Option<Artifact> {
        let file = self.settings.report.clone()?;
        if !file.contains("://") || file.starts_with("file:") {
            return None;
        }
        let registration = Registration {
            direction: None,
            choice: Choice {
                kind: Some("__lamindb_run__".into()),
                labels: Vec::new(),
            },
            description: format!("Nextflow execution report for run {}", run.run_uid),
        };
        let artifact = self.register(&file, registration)?;
        tracing::info!("Created report artifact {}", artifact.uid);
        Some(artifact)
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

/// The path as Nextflow's S3 paths print their address, with three slashes.
fn nextflow_uri(path: &str) -> String {
    match path.strip_prefix("s3://") {
        Some(rest) => format!("s3:///{rest}"),
        None => path.to_owned(),
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
        assert_eq!(nextflow_uri("s3://b/k.csv"), "s3:///b/k.csv");
        assert_eq!(nextflow_uri("gs://b/k.csv"), "gs://b/k.csv");
    }
}
