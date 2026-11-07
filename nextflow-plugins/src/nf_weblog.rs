//! nf-weblog 1.2.0: posts a JSON message for each run and task event to an HTTP address.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashSet;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use jiff::Timestamp;
use jiff::civil::Weekday;
use jiff::tz::TimeZone;
use nextflow_config::Config;
use nextflow_evaluator::value::Value;

use crate::catalogue::{Kind, Plugin, State};
use crate::observer::{Background, End, Event, Observer, Task};
use crate::report_format::{Json, now};
use crate::run_metadata::{Run, Stats, TRACE_FIELDS, trace_value};
use crate::service::{self, Sender};

pub(crate) const PLUGIN: Plugin = Plugin {
    scopes: &["weblog"],
    observer: Some(activate),
    ..Plugin::new("nf-weblog", Kind::Observer, State::Implemented)
};

/// How long one message may take.
const LIMIT: Duration = Duration::from_secs(30);
/// How long the end of the run waits for the messages that are left.
const WAIT: Duration = Duration::from_secs(60);
/// The exit status of a task that has not ended, Java's largest `int`.
const UNKNOWN_EXIT: i64 = 2_147_483_647;

fn activate(config: &Config) -> Result<Option<Box<dyn Observer>>, String> {
    let setting =
        |key: &str| (config.get(&["weblog", key])).filter(|value| !matches!(value, Value::Null));
    if !setting("enabled").is_some_and(|value| value.truth()) {
        return Ok(None);
    }
    let url = setting("url").map_or("http://localhost".to_owned(), |url| url.to_string());
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(format!(
            "Only http and https are supported -- The given URL was: {}",
            service::safe_url(&url)
        ));
    }
    let auth = match setting("basicToken").map(|token| token.to_string()) {
        Some(token) if !token.contains(':') => {
            return Err("Invalid auth token provided - should be 'USERNAME:PASSWORD'.".to_owned());
        }
        token => token.map(|token| format!("Basic {}", STANDARD.encode(token))),
    };
    let mut weblog = Weblog {
        url,
        auth,
        sender: None,
        run: None,
        warned: HashSet::new(),
    };
    let background = Background::start("nf-weblog", WAIT, move |event| weblog.handle(&event))
        .map_err(|error| error.to_string())?;
    Ok(Some(Box::new(background)))
}

/// Sends the messages in the worker thread, in the order of the events.
struct Weblog {
    url: String,
    /// The `Authorization` header of `basicToken`.
    auth: Option<String>,
    sender: Option<Sender>,
    run: Option<Run>,
    /// The connection problems already shown, each once.
    warned: HashSet<String>,
}

impl Weblog {
    fn handle(&mut self, event: &Event) {
        if let Some(run) = &mut self.run {
            run.stats.count(event);
        }
        match event {
            Event::RunStarted(start) => {
                self.run = Some(Run::new(start));
                self.started();
            }
            Event::TaskSubmitted(task) => self.post("process_submitted", "trace", trace(task)),
            Event::TaskStarted(task) => self.post("process_started", "trace", trace(task)),
            Event::TaskCompleted(task) | Event::TaskFailed(task) => {
                self.post("process_completed", "trace", trace(task));
            }
            Event::RunEnded(end) => self.ended(end),
            Event::TaskCached(_) | Event::FilePublished { .. } => {}
        }
    }

    /// The run starts: its params are those of the config and the command line, and its
    /// statistics are still empty.
    fn started(&mut self) {
        let Some(run) = &self.run else {
            return;
        };
        let metadata = metadata(run, &Stats::default());
        self.post("started", "metadata", metadata);
    }

    /// A failed run sends the error with the failing task first, then the run's end once,
    /// where Nextflow 26.04.6 sends the end twice and the error last without its task.
    fn ended(&mut self, end: &End) {
        let Some(run) = &mut self.run else {
            return;
        };
        run.end(end);
        if !end.success {
            match &end.task {
                Some(task) => self.post("error", "trace", trace(task)),
                None => self.send(message("error", None, &self.run)),
            }
        }
        let Some(run) = &self.run else {
            return;
        };
        let metadata = metadata(run, &run.stats);
        self.post("completed", "metadata", metadata);
    }

    fn post(&mut self, event: &str, field: &str, value: Json) {
        let body = message(event, Some((field, value)), &self.run);
        self.send(body);
    }

    /// Posts one message, once; an answer that is not a success is only logged, and each
    /// connection problem is shown once. The address may hold credentials and is not logged.
    fn send(&mut self, body: Json) {
        let sender = match service::sender(&mut self.sender) {
            Ok(sender) => sender,
            Err(error) => return tracing::warn!("nf-weblog: {error}"),
        };
        let mut request = (sender.client().post(&self.url))
            .timeout(LIMIT)
            .header("Content-Type", "application/json")
            .body(body.compact());
        if let Some(auth) = &self.auth {
            request = request.header("Authorization", auth);
        }
        let problem = match sender.send(request) {
            Ok(answer) if (200..300).contains(&answer.status) => return,
            Ok(answer) => {
                return tracing::debug!(
                    "Unexpected HTTP response.\nFailed to send message to the weblog address -- \
                     received\n- status code : {}\n- response msg: {}",
                    answer.status,
                    answer.body
                );
            }
            Err(error) if error.is_connect() => "Connection refused",
            Err(error) if error.is_timeout() => "Connection timed out",
            Err(_) => "The weblog request failed",
        };
        if self.warned.insert(problem.to_owned()) {
            tracing::warn!("nf-weblog: {problem}");
        }
    }
}

/// A message: the trace or metadata, then the run, the event and the time of sending.
fn message(event: &str, value: Option<(&str, Json)>, run: &Option<Run>) -> Json {
    let start = run.as_ref().map(|run| &run.start);
    let mut entries = Vec::new();
    if let Some((field, value)) = value {
        entries.push((field.to_owned(), value));
    }
    entries.extend([
        (
            "runId".into(),
            Json::from(start.map(|start| start.session_id.clone())),
        ),
        ("event".into(), Json::from(event)),
        (
            "runName".into(),
            Json::from(start.map(|start| start.run_name.clone())),
        ),
        ("utcTime".into(), Json::from(utc_time(now()))),
    ]);
    Json::Object(entries)
}

/// The params and `workflow` of the run, with times as Java's date objects and the statistics
/// after the config files, as the plugin sends them.
fn metadata(run: &Run, stats: &Stats) -> Json {
    let Json::Object(mut entries) = run.workflow(date) else {
        return Json::Null;
    };
    for (key, value) in &mut entries {
        if key == "container" && *value == Json::Null {
            *value = Json::object::<&str>([]);
        }
    }
    let place = entries.iter().position(|(key, _)| key == "configFiles");
    let place = place.map_or(entries.len(), |place| place + 1);
    entries.insert(place, ("stats".into(), stats.workflow(true)));
    Json::object([
        ("parameters", Json::from(&run.start.params)),
        ("workflow", Json::Object(entries)),
    ])
}

/// The trace record with Nextflow's field names: times in milliseconds, `start` 0 before the
/// task starts and `exit` Java's largest `int` before it ends; the fields of the end follow
/// once it ended. Nextflow 26.04.6 sends no resource usage, so neither does uwe.
fn trace(task: &Task) -> Json {
    let status = task.record.get("status").and_then(|status| status.as_str());
    let ended = matches!(status, Some("COMPLETED" | "FAILED" | "CACHED"));
    let mut entries = Vec::new();
    for field in TRACE_FIELDS {
        let value = trace_value(task, field);
        let value = match field {
            "start" if value.is_null() => Json::from(0i64),
            "exit" if value.is_null() || !ended => Json::from(UNKNOWN_EXIT),
            "error_action" | "complete" | "duration" | "realtime" if !ended => continue,
            "native_id" if value.is_null() => continue,
            _ => Json::from(&value),
        };
        entries.push((field.to_owned(), value));
        if field == "realtime" || (field == "accelerator_type" && !ended) {
            break;
        }
    }
    let native = trace_value(task, "native_id");
    if !native.is_null() {
        entries.push(("native_id".into(), Json::from(&native)));
    }
    Json::Object(entries)
}

/// A time as Java's `OffsetDateTime` object written field by field, in the time zone of
/// this machine.
fn date(millis: i64) -> Json {
    let Ok(time) = Timestamp::from_millisecond(millis) else {
        return Json::Null;
    };
    let time = time.to_zoned(TimeZone::system());
    let seconds = time.offset().seconds();
    let id = match seconds {
        0 => "Z".to_owned(),
        _ => time.strftime("%:z").to_string(),
    };
    let weekday = match time.weekday() {
        Weekday::Monday => "MONDAY",
        Weekday::Tuesday => "TUESDAY",
        Weekday::Wednesday => "WEDNESDAY",
        Weekday::Thursday => "THURSDAY",
        Weekday::Friday => "FRIDAY",
        Weekday::Saturday => "SATURDAY",
        Weekday::Sunday => "SUNDAY",
    };
    const MONTHS: [&str; 12] = [
        "JANUARY",
        "FEBRUARY",
        "MARCH",
        "APRIL",
        "MAY",
        "JUNE",
        "JULY",
        "AUGUST",
        "SEPTEMBER",
        "OCTOBER",
        "NOVEMBER",
        "DECEMBER",
    ];
    let month = MONTHS[usize::try_from(time.month() - 1).unwrap_or_default()];
    let rules = Json::object([
        ("fixedOffset", Json::from(true)),
        ("transitionRules", Json::List(Vec::new())),
        ("transitions", Json::List(Vec::new())),
    ]);
    let offset = Json::object([
        ("id", Json::from(id)),
        ("rules", rules),
        ("totalSeconds", Json::from(i64::from(seconds))),
    ]);
    let number = |value: i64| Json::from(value);
    Json::object([
        ("dayOfMonth", number(time.day().into())),
        ("dayOfWeek", Json::from(weekday)),
        ("dayOfYear", number(time.day_of_year().into())),
        ("hour", number(time.hour().into())),
        ("minute", number(time.minute().into())),
        ("month", Json::from(month)),
        ("monthValue", number(time.month().into())),
        ("nano", number(millis.rem_euclid(1000) * 1_000_000)),
        ("offset", offset),
        ("second", number(time.second().into())),
        ("year", number(time.year().into())),
    ])
}

/// A time in UTC in whole seconds, like `2026-11-06T11:48:43Z`.
fn utc_time(millis: i64) -> String {
    match Timestamp::from_millisecond(millis) {
        Ok(time) => time.strftime("%Y-%m-%dT%H:%M:%SZ").to_string(),
        Err(_) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_java_times() {
        assert_eq!(utc_time(1_793_965_723_000), "2026-11-06T11:48:43Z");
        let text = date(1_793_965_723_247).compact();
        assert!(
            text.contains(r#""dayOfWeek":"FRIDAY","dayOfYear":310,"#),
            "{text}"
        );
        assert!(text.contains(r#""month":"NOVEMBER","monthValue":11,"nano":247000000,"#));
        assert!(text.contains(r#""rules":{"fixedOffset":true,"transitionRules":[],"#));
    }
}
