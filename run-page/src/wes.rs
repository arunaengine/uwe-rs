//! The GA4GH Workflow Execution Service, versions 1.0 and 1.1, over the studio and run store.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use run_database::{Delivered, Run, TaskRow};
use serde_json::{Map, Value, json};

use crate::api::public;
use crate::launch::resumable;
use crate::studio::{LaunchRequest, Profiles, ResumeFrom};
use crate::{Failure, Launched, Phase, Refusal, Shared, Studio, query};

/// The workflow types of WES by the language names of uwe, with the versions uwe follows; a
/// Galaxy workflow is a `.ga` file of `format-version` 0.1.
const TYPES: [(&str, &str, &[&str]); 7] = [
    ("nextflow", "NFL", &["DSL2"]),
    ("snakemake", "SMK", &["9.27"]),
    ("cwl", "CWL", &["v1.0", "v1.1", "v1.2"]),
    ("argo", "ARGO", &["v1alpha1"]),
    ("wdl", "WDL", &["1.0", "1.1", "1.2"]),
    ("galaxy", "GALAXY", &["0.1"]),
    ("wir", "WIR", &["0.1", "0.2", "0.3"]),
];

/// The engine parameters that a submission may set, as the launcher of the page sets them.
const ENGINE: [(&str, &str); 9] = [
    ("executor", "local"),
    ("profile", ""),
    ("entry", ""),
    ("preset", ""),
    ("default_image", ""),
    ("output_dir", ""),
    ("resume", "false"),
    ("no_network", "false"),
    ("no_cache", "false"),
];

/// How many runs a page of the list holds unless the request says otherwise.
const PAGE: usize = 100;

pub(crate) fn routes() -> Router<Shared> {
    Router::new()
        .route("/ga4gh/wes/v1/service-info", get(service_info))
        .route("/ga4gh/wes/v1/runs", get(list).post(submit))
        .route("/ga4gh/wes/v1/runs/{run}", get(run_log))
        .route("/ga4gh/wes/v1/runs/{run}/status", get(status))
        .route("/ga4gh/wes/v1/runs/{run}/cancel", post(cancel))
}

/// A WES error: `{"msg": ..., "status_code": ...}` with the same status.
struct Problem {
    status: StatusCode,
    message: String,
}

impl From<Failure> for Problem {
    fn from(failure: Failure) -> Problem {
        Problem {
            status: failure.status,
            message: failure.message,
        }
    }
}

impl From<Refusal> for Problem {
    fn from(refusal: Refusal) -> Problem {
        Problem {
            status: refusal.status,
            message: refusal.message,
        }
    }
}

impl Problem {
    fn invalid(message: impl Into<String>) -> Problem {
        Problem {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }
}

impl IntoResponse for Problem {
    fn into_response(self) -> Response {
        let message = run_database::redact_public_text(&self.message);
        let body = json!({"msg": message, "status_code": self.status.as_u16()});
        (self.status, Json(body)).into_response()
    }
}

async fn service_info(State(state): State<Shared>) -> Result<Json<Value>, Problem> {
    let offered = state.studio()?.capabilities();
    let mut types = Map::new();
    for (language, name, versions) in TYPES {
        if offered.languages.iter().any(|known| known == language) {
            types.insert(name.into(), json!({"workflow_type_version": versions}));
        }
    }
    let mut counts: BTreeMap<&str, u64> = BTreeMap::new();
    for summary in summaries(&state).await? {
        *counts.entry(summary.state).or_default() += 1;
    }
    let defaults: Vec<Value> = (ENGINE.iter())
        .map(|(name, value)| json!({"name": name, "type": "string", "default_value": value}))
        .collect();
    let version = env!("CARGO_PKG_VERSION");
    Ok(Json(json!({
        "id": "uwe.wes",
        "name": "uwe",
        "type": {"group": "org.ga4gh", "artifact": "wes", "version": "1.1.0"},
        "description": "Runs Nextflow, Snakemake, CWL, Argo, WDL and Galaxy workflows with uwe.",
        "organization": {"name": "The uwe Contributors", "url": "https://github.com/uwe-rs"},
        "version": version,
        "workflow_type_versions": types,
        "supported_wes_versions": ["1.0.0", "1.1.0"],
        "supported_filesystem_protocols": ["file", "http", "https", "s3", "gs", "az"],
        "workflow_engine_versions": {"uwe": version},
        "default_workflow_engine_parameters": defaults,
        "system_state_counts": counts,
        "auth_instructions_url": "",
        "tags": {"executors": offered.executors.join(",")},
    })))
}

/// One run of the list, with its WES state.
struct Summary {
    run: String,
    state: &'static str,
    started: Option<i64>,
    ended: Option<i64>,
    tags: BTreeMap<String, Value>,
}

/// Every run that this page shows: those that the studio started and the store does not hold
/// yet, then the runs of the store, the latest first.
async fn summaries(state: &Shared) -> Result<Vec<Summary>, Problem> {
    let studio = state.studio()?;
    let launches = studio.launches();
    let allowed = state.allowed();
    let stored = query(state, move |database| {
        let Some(allowed) = allowed else {
            return Ok(database.runs(u32::MAX)?);
        };
        let mut runs = Vec::new();
        for run in allowed {
            runs.extend(database.run(&run)?);
        }
        runs.sort_by(|a, b| b.started.cmp(&a.started).then_with(|| b.id.cmp(&a.id)));
        Ok(runs)
    })
    .await?;
    let mut found = Vec::new();
    for launched in launches
        .iter()
        .filter(|launched| state.permits(&launched.run))
    {
        if !stored.iter().any(|run| run.id == launched.run) {
            found.push(Summary {
                run: launched.run.clone(),
                state: wes_state(None, Some(launched)),
                started: Some(launched.time),
                ended: None,
                tags: launched.tags(),
            });
        }
    }
    for run in &stored {
        let launched = launches.iter().find(|launched| launched.run == run.id);
        found.push(Summary {
            run: run.id.clone(),
            state: wes_state(Some(run), launched),
            started: Some(run.started),
            ended: run.ended,
            tags: launched.map(Launched::tags).unwrap_or_default(),
        });
    }
    Ok(found)
}

#[derive(serde::Deserialize)]
struct Paging {
    page_size: Option<usize>,
    /// The offset of the page in the list, as the last page gave it.
    page_token: Option<String>,
}

async fn list(
    State(state): State<Shared>,
    Query(paging): Query<Paging>,
) -> Result<Json<Value>, Problem> {
    let size = paging.page_size.filter(|size| *size > 0).unwrap_or(PAGE);
    let start: usize = match paging
        .page_token
        .as_deref()
        .filter(|token| !token.is_empty())
    {
        None => 0,
        Some(token) => (token.parse()).map_err(|_| Problem::invalid("unknown page_token"))?,
    };
    let found = summaries(&state).await?;
    let end = start.saturating_add(size);
    let next = (end < found.len()).then(|| end.to_string());
    let runs: Vec<Value> = (found.iter().skip(start).take(size))
        .map(|summary| {
            json!({
                "run_id": summary.run,
                "state": summary.state,
                "start_time": summary.started.map(iso),
                "end_time": summary.ended.map(iso),
                "tags": summary.tags,
            })
        })
        .collect();
    let body = json!({"runs": runs, "next_page_token": next.unwrap_or_default()});
    Ok(public(&body)?)
}

/// Starts a run from a JSON body, or from the fields of a form as WES clients send them.
async fn submit(
    State(state): State<Shared>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, Problem> {
    let studio = state.studio()?;
    let kind = (headers.get(header::CONTENT_TYPE))
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/json");
    let request = match kind.split(';').next().unwrap_or_default().trim() {
        "application/json" => match serde_json::from_slice(&body) {
            Ok(Value::Object(request)) => request,
            _ => return Err(Problem::invalid("the body is no JSON object")),
        },
        "application/x-www-form-urlencoded" => fields(form(&body))?,
        "multipart/form-data" => {
            let boundary = parameter(kind, "boundary")
                .ok_or_else(|| Problem::invalid("the form names no boundary"))?;
            fields(multipart(&boundary, &body)?)?
        }
        other => {
            return Err(Problem::invalid(format!(
                "uwe reads no body of type {other}"
            )));
        }
    };
    let request = resumable(&state, launch_request(&request, studio)?)?;
    let started = studio.launch(request).await?;
    state.admit(&started.run_id);
    Ok(Json(json!({"run_id": started.run_id})))
}

/// The request of the launcher for a WES submission, which it keeps as `wes`.
fn launch_request(
    request: &Map<String, Value>,
    studio: &dyn Studio,
) -> Result<LaunchRequest, Problem> {
    let url = (request.get("workflow_url").and_then(Value::as_str))
        .filter(|url| !url.is_empty())
        .ok_or_else(|| Problem::invalid("the request has no workflow_url"))?;
    let mut launch = LaunchRequest {
        target: Some(url.strip_prefix("file://").unwrap_or(url).to_owned()),
        wes: Some(Value::Object(request.clone())),
        ..LaunchRequest::default()
    };
    match request.get("workflow_params") {
        None | Some(Value::Null) => {}
        Some(Value::Object(values)) => launch.values = Some(values.clone()),
        Some(_) => return Err(Problem::invalid("workflow_params is no JSON object")),
    }
    if let Some(kind) = request.get("workflow_type").and_then(Value::as_str) {
        launch.language = Some(language(kind, studio)?.to_owned());
    }
    match request.get("workflow_engine_parameters") {
        None | Some(Value::Null) => {}
        Some(Value::Object(engine)) => {
            for (name, value) in engine {
                set_engine(&mut launch, name, value)?;
            }
        }
        Some(_) => {
            return Err(Problem::invalid(
                "workflow_engine_parameters is no JSON object",
            ));
        }
    }
    Ok(launch)
}

/// The language of uwe of a WES workflow type, when this server runs it.
fn language(kind: &str, studio: &dyn Studio) -> Result<&'static str, Problem> {
    let language = (TYPES.iter())
        .find(|(language, name, _)| name.eq_ignore_ascii_case(kind) || *language == kind)
        .map(|(language, _, _)| *language)
        .ok_or_else(|| Problem::invalid(format!("uwe runs no workflow_type {kind}")))?;
    let offered = studio.capabilities().languages;
    if !offered.iter().any(|known| known == language) {
        return Err(Problem::invalid(format!(
            "this server runs no {kind} workflows"
        )));
    }
    Ok(language)
}

/// Sets an engine parameter of a submission. They are texts in WES; the flags of the launcher
/// are booleans, which a text `true` sets.
fn set_engine(launch: &mut LaunchRequest, name: &str, value: &Value) -> Result<(), Problem> {
    let text = match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    };
    let flag = Some(text == "true");
    match name {
        "executor" => launch.executor = Some(text),
        "profile" => launch.profile = Some(Profiles::Text(text)),
        "entry" => launch.entry = Some(text),
        "preset" => launch.preset = Some(text),
        "default_image" => launch.default_image = Some(text),
        "output_dir" => launch.output_dir = Some(text),
        "resume" => launch.resume = Some(ResumeFrom::Run(text)),
        "no_network" => launch.no_network = flag,
        "no_cache" => launch.no_cache = flag,
        _ => {
            let known: Vec<&str> = ENGINE.iter().map(|(name, _)| *name).collect();
            return Err(Problem::invalid(format!(
                "unknown engine parameter {name}; uwe takes {}",
                known.join(", ")
            )));
        }
    }
    Ok(())
}

/// The fields of a form, with the JSON of `workflow_params`, `workflow_engine_parameters`
/// and `tags` read.
fn fields(pairs: Vec<(String, String)>) -> Result<Map<String, Value>, Problem> {
    let mut request = Map::new();
    for (name, text) in pairs {
        let value = match name.as_str() {
            "workflow_params" | "workflow_engine_parameters" | "tags" => {
                (serde_json::from_str(&text))
                    .map_err(|_| Problem::invalid(format!("{name} is no JSON")))?
            }
            "workflow_attachment" => {
                return Err(Problem::invalid(
                    "uwe takes no workflow_attachment; give workflow_url as a path or address",
                ));
            }
            _ => Value::String(text),
        };
        request.insert(name, value);
    }
    Ok(request)
}

async fn run_log(
    State(state): State<Shared>,
    Path(id): Path<String>,
) -> Result<Json<Value>, Problem> {
    let found = found(&state, &id).await?;
    let launched = found.launched.as_ref();
    let shown = wes_state(found.run.as_ref(), launched);
    let request = match (launched, &found.run) {
        (Some(launched), _) => launched.request.wes.clone().unwrap_or_else(|| {
            let asked = &launched.request;
            json!({"workflow_url": asked.target, "workflow_params": asked.values})
        }),
        (None, Some(run)) => json!({
            "workflow_url": run.workflow,
            "workflow_type": wes_type(&run.language),
            "workflow_params": run.parameters.iter().cloned().collect::<BTreeMap<_, _>>(),
        }),
        (None, None) => Value::Null,
    };
    let stored = found.run.as_ref();
    let error = (stored.and_then(|run| run.error.clone()))
        .or_else(|| launched.and_then(|launched| launched.error.clone()));
    let command = |run: &Run| {
        let words = std::iter::once("uwe".to_owned()).chain(run.arguments.iter().cloned());
        words.collect::<Vec<_>>()
    };
    let started =
        (stored.map(|run| run.started)).or_else(|| launched.map(|launched| launched.time));
    let run_log = json!({
        "name": stored.map(|run| base(&run.workflow)),
        "cmd": stored.map(command),
        "start_time": started.map(iso),
        "end_time": stored.and_then(|run| run.ended).map(iso),
        "stdout": null,
        "stderr": null,
        "exit_code": null,
        "system_logs": error.into_iter().collect::<Vec<_>>(),
    });
    let task_logs: Vec<Value> = (found.tasks.iter())
        .map(|task| task_log(&id, task))
        .collect();
    let outputs: Map<String, Value> = (found.delivered.iter())
        .map(|delivered| (delivered.target.clone(), json!(delivered.location)))
        .collect();
    let body = json!({
        "run_id": id,
        "request": request,
        "state": shown,
        "run_log": run_log,
        "task_logs": task_logs,
        "task_logs_url": null,
        "outputs": outputs,
    });
    Ok(Json(run_database::public_json(&body).unwrap_or(body)))
}

fn task_log(run: &str, task: &TaskRow) -> Value {
    let logs = |stream: &str| {
        (task.attempts > 0).then(|| {
            let run = run.replace('/', "%2F");
            format!(
                "/api/runs/{run}/tasks/{}/logs/{}/{stream}",
                task.id, task.attempts
            )
        })
    };
    json!({
        "id": task.id.to_string(),
        "name": task.name,
        "cmd": [],
        "start_time": task.started.map(iso),
        "end_time": task.ended.map(iso),
        "exit_code": task.exit_code,
        "stdout": logs("stdout"),
        "stderr": logs("stderr"),
        "system_logs": [],
        "tes_uri": null,
    })
}

async fn status(
    State(state): State<Shared>,
    Path(id): Path<String>,
) -> Result<Json<Value>, Problem> {
    let (run, launched) = known(&state, &id).await?;
    let shown = wes_state(run.as_ref(), launched.as_ref());
    Ok(Json(json!({"run_id": id, "state": shown})))
}

async fn cancel(
    State(state): State<Shared>,
    Path(id): Path<String>,
) -> Result<Json<Value>, Problem> {
    let (run, launched) = known(&state, &id).await?;
    if state.studio()?.cancel(&id) {
        return Ok(Json(json!({"run_id": id})));
    }
    let shown = wes_state(run.as_ref(), launched.as_ref());
    Err(Problem {
        status: StatusCode::CONFLICT,
        message: format!("run {id} is {shown} and does not go on in this server"),
    })
}

/// What the store and the studio know of one run.
struct Found {
    run: Option<Run>,
    tasks: Vec<TaskRow>,
    delivered: Vec<Delivered>,
    launched: Option<Launched>,
}

async fn found(state: &Shared, id: &str) -> Result<Found, Problem> {
    let (stored, launched) = known(state, id).await?;
    let run = id.to_owned();
    let (tasks, delivered) = match stored {
        None => (Vec::new(), Vec::new()),
        Some(_) => {
            let read = query(state, move |database| {
                Ok((database.tasks(&run)?, database.deliveries(&run)?))
            });
            read.await?
        }
    };
    Ok(Found {
        run: stored,
        tasks,
        delivered,
        launched,
    })
}

/// The row of a run in the store and what the studio knows of it, without its tasks.
async fn known(state: &Shared, id: &str) -> Result<(Option<Run>, Option<Launched>), Problem> {
    state.authorize(id)?;
    let launched = state.studio()?.launched(id);
    let run = id.to_owned();
    let stored = query(state, move |database| Ok(database.run(&run)?)).await?;
    if stored.is_none() && launched.is_none() {
        return Err(Failure::not_found(format!("no run `{id}`")).into());
    }
    Ok((stored, launched))
}

/// The WES state of a run: the state of the store once it holds the run, else how far the
/// studio has come with it.
fn wes_state(run: Option<&Run>, launched: Option<&Launched>) -> &'static str {
    let phase = launched.map(|launched| launched.phase);
    match (run.map(|run| run.state.as_str()), phase) {
        (Some("running") | None, Some(Phase::Canceling)) => "CANCELING",
        (Some("running"), _) => "RUNNING",
        (Some("succeeded"), _) => "COMPLETE",
        (Some("failed"), _) => "EXECUTOR_ERROR",
        (Some("stopped"), _) => "CANCELED",
        (Some(_), _) => "UNKNOWN",
        (None, Some(Phase::Queued)) => "QUEUED",
        (None, Some(Phase::Running)) => "INITIALIZING",
        (None, Some(Phase::Complete)) => "COMPLETE",
        (None, Some(Phase::Failed)) => "EXECUTOR_ERROR",
        (None, Some(Phase::Canceled)) => "CANCELED",
        (None, None) => "UNKNOWN",
    }
}

fn wes_type(language: &str) -> &str {
    (TYPES.iter())
        .find(|(name, _, _)| *name == language)
        .map_or(language, |(_, kind, _)| kind)
}

fn base(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_owned()
}

/// A time in milliseconds since the Unix epoch in RFC 3339 form, in UTC.
fn iso(milliseconds: i64) -> String {
    let seconds = milliseconds.div_euclid(1000);
    let (days, rest) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    // The civil date of a day count, as in the days_from_civil algorithm of Howard Hinnant.
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let of_era = shifted.rem_euclid(146_097);
    let year_of_era = (of_era - of_era / 1460 + of_era / 36_524 - of_era / 146_096) / 365;
    let day_of_year = of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    let (hour, minute, second) = (rest / 3600, rest % 3600 / 60, rest % 60);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// The pairs of an `application/x-www-form-urlencoded` body.
fn form(body: &[u8]) -> Vec<(String, String)> {
    let text = String::from_utf8_lossy(body);
    (text.split('&').filter(|pair| !pair.is_empty()))
        .map(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            (decoded(name), decoded(value))
        })
        .collect()
}

/// A form text with `+` as a space and `%XX` as its byte.
fn decoded(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let hex = |offset: usize| {
            (bytes
                .get(at + offset)
                .map(|byte| (*byte as char).to_digit(16)))?
        };
        let escaped = (bytes[at] == b'%').then(|| hex(1).zip(hex(2))).flatten();
        match (bytes[at], escaped) {
            (_, Some((high, low))) => {
                out.push((high * 16 + low) as u8);
                at += 2;
            }
            (b'+', None) => out.push(b' '),
            (byte, None) => out.push(byte),
        }
        at += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The text fields of a `multipart/form-data` body; a part that holds a file is refused.
fn multipart(boundary: &str, body: &[u8]) -> Result<Vec<(String, String)>, Problem> {
    let text = String::from_utf8_lossy(body);
    let delimiter = format!("--{boundary}");
    let mut fields = Vec::new();
    for part in text.split(delimiter.as_str()).skip(1) {
        if part.starts_with("--") {
            break;
        }
        let part = part.strip_prefix("\r\n").unwrap_or(part);
        let (head, value) = (part.split_once("\r\n\r\n"))
            .ok_or_else(|| Problem::invalid("a part of the form has no header"))?;
        let disposition = (head.lines())
            .find(|line| {
                line.to_ascii_lowercase()
                    .starts_with("content-disposition:")
            })
            .ok_or_else(|| Problem::invalid("a part of the form has no name"))?;
        let name = parameter(disposition, "name")
            .ok_or_else(|| Problem::invalid("a part of the form has no name"))?;
        if parameter(disposition, "filename").is_some() {
            return Err(Problem::invalid(
                "uwe takes no workflow_attachment; give workflow_url as a path or address",
            ));
        }
        fields.push((name, value.strip_suffix("\r\n").unwrap_or(value).to_owned()));
    }
    Ok(fields)
}

/// A parameter of a header value, like `boundary` of `multipart/form-data; boundary=x`.
fn parameter(header: &str, key: &str) -> Option<String> {
    header.split(';').skip(1).find_map(|item| {
        let (name, value) = item.split_once('=')?;
        (name.trim().eq_ignore_ascii_case(key)).then(|| value.trim().trim_matches('"').to_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_utc_times() {
        assert_eq!(iso(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso(1_795_437_296_789), "2026-11-23T12:34:56Z");
        assert_eq!(iso(951_782_400_000), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn reads_form_bodies() {
        let pairs =
            form(b"workflow_url=main.nf&workflow_params=%7B%22a%22%3A1%7D&tags=%7B%7D&x=a+b");
        assert_eq!(pairs[0], ("workflow_url".into(), "main.nf".into()));
        assert_eq!(pairs[1].1, "{\"a\":1}");
        assert_eq!(pairs[3].1, "a b");
        let body = "--b\r\nContent-Disposition: form-data; name=\"workflow_url\"\r\n\r\nmain.nf\r\n\
                    --b\r\nContent-Disposition: form-data; name=\"tags\"\r\n\r\n{}\r\n--b--\r\n";
        let fields = multipart("b", body.as_bytes())
            .ok()
            .expect("the fields of the form");
        assert_eq!(
            fields,
            [
                ("workflow_url".into(), "main.nf".into()),
                ("tags".into(), "{}".into())
            ]
        );
        let file = "--b\r\nContent-Disposition: form-data; name=\"workflow_attachment\"; \
                    filename=\"main.nf\"\r\n\r\nx\r\n--b--\r\n";
        assert!(multipart("b", file.as_bytes()).is_err());
    }
}
