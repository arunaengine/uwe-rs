//! nf-teams 0.2.2: posts Adaptive Cards to a Microsoft Teams webhook when a run starts, ends,
//! succeeds or fails, from built-in cards or Groovy templates.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use nextflow_config::Config;
use nextflow_evaluator::interpreter::{Host, Interpreter};
use nextflow_evaluator::number::Number;
use nextflow_evaluator::value::Value;
use nextflow_parser::Span;

use crate::catalogue::{Kind, Plugin, State};
use crate::observer::{Background, End, Event, Observer};
use crate::report_format::{Json, offset_time};
use crate::run_metadata::{NEXTFLOW_BUILD, NEXTFLOW_TIMESTAMP, Run};
use crate::service::{self, Sender};

pub(crate) const PLUGIN: Plugin = Plugin {
    functions: &["sayHello"],
    call: Some(crate::nf_storagemetrics::say_hello),
    scopes: &["teams"],
    observer: Some(activate),
    ..Plugin::new("nf-teams", Kind::Observer, State::Implemented)
};

/// How long one message may take.
const LIMIT: Duration = Duration::from_secs(30);
/// How long the end of the run waits for the messages that are left.
const WAIT: Duration = Duration::from_secs(60);
const SCHEMA: &str = "http://adaptivecards.io/schemas/adaptive-card.json";

/// The cards of the four events.
#[derive(Clone, Copy, PartialEq)]
enum Card {
    Start,
    Complete,
    Success,
    Error,
}

/// An enabled card and its template file, `None` for the built-in card.
struct Enabled {
    card: Card,
    template: Option<String>,
}

fn activate(config: &Config) -> Result<Option<Box<dyn Observer>>, String> {
    let get = |path: &[&str]| {
        let mut full = vec!["teams"];
        full.extend_from_slice(path);
        config
            .get(&full)
            .filter(|value| !matches!(value, Value::Null))
    };
    if !get(&["enabled"]).is_some_and(|value| value.truth()) {
        return Ok(None);
    }
    let url = get(&["webHook", "url"]).map(|url| url.to_string());
    let valid = |url: &str| {
        let rest = (url.strip_prefix("http://")).or_else(|| url.strip_prefix("https://"));
        rest.is_some_and(|rest| !rest.is_empty())
    };
    let Some(url) = url.filter(|url| valid(url)) else {
        tracing::warn!(
            "Microsoft Teams notifications are enabled but the webhook URL is invalid. Please \
             check your configuration."
        );
        return Ok(None);
    };
    let scopes = [
        ("onStart", Card::Start),
        ("onComplete", Card::Complete),
        ("onSuccess", Card::Success),
        ("onError", Card::Error),
    ];
    let events = (scopes.into_iter())
        .filter(|(scope, _)| get(&[scope, "enabled"]).is_some_and(|value| value.truth()))
        .map(|(scope, card)| Enabled {
            card,
            template: get(&[scope, "template"]).map(|path| path.to_string()),
        })
        .collect();
    let mut teams = Teams {
        url,
        events,
        sender: None,
        run: None,
    };
    let background = Background::start("nf-teams", WAIT, move |event| teams.handle(&event))
        .map_err(|error| error.to_string())?;
    Ok(Some(Box::new(background)))
}

/// Sends the cards in the worker thread.
struct Teams {
    url: String,
    events: Vec<Enabled>,
    sender: Option<Sender>,
    run: Option<Run>,
}

impl Teams {
    /// Sends each enabled card once: the start; at the end the error card of a failed run
    /// first, then the complete card, then the success card of a run that succeeded.
    fn handle(&mut self, event: &Event) {
        match event {
            Event::RunStarted(start) => {
                self.run = Some(Run::new(start));
                self.send(Card::Start, None);
            }
            Event::RunEnded(end) => {
                if let Some(run) = &mut self.run {
                    run.end(end);
                }
                if !end.success {
                    self.send(Card::Error, Some(end));
                }
                self.send(Card::Complete, Some(end));
                if end.success {
                    self.send(Card::Success, Some(end));
                }
            }
            _ => {}
        }
    }

    fn send(&mut self, card: Card, end: Option<&End>) {
        let Some(event) = self.events.iter().find(|event| event.card == card) else {
            return;
        };
        let Some(run) = &self.run else {
            return;
        };
        let what = match card {
            Card::Start => "start",
            Card::Complete => "completion",
            Card::Success => "success",
            Card::Error => "error",
        };
        tracing::info!("Sending Teams notification on workflow {what}");
        let body = match &event.template {
            None => built_in(card, run).compact(),
            Some(template) => match render(template, run, end) {
                Ok(text) => text,
                Err(error) => {
                    return tracing::warn!(
                        "The Teams template {template} was not rendered: {error}"
                    );
                }
            },
        };
        tracing::info!("Sending message to Teams webhook");
        let sender = match service::sender(&mut self.sender) {
            Ok(sender) => sender,
            Err(error) => return tracing::warn!("nf-teams: {error}"),
        };
        let request = (sender.client().post(&self.url))
            .timeout(LIMIT)
            .header("Content-Type", "application/json")
            .body(body);
        match sender.send(request) {
            Ok(answer) if (200..300).contains(&answer.status) => {}
            Ok(answer) => tracing::warn!("{}", answer.body),
            Err(error) => tracing::warn!("nf-teams: {}", error.without_url()),
        }
    }
}

/// A built-in card. Values are escaped, where the plugin pastes them into JSON text as they
/// are, which breaks the card for a `"` or a line break.
fn built_in(card: Card, run: &Run) -> Json {
    let start = &run.start;
    let ending = run.ending.as_ref();
    let manifest = |key: &str| match &start.manifest[key] {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Null => "null".to_owned(),
        other => other.to_string(),
    };
    let kind = match card {
        Card::Start => "Started",
        Card::Complete => "Completed",
        Card::Success => "Success",
        Card::Error => "Failed",
    };
    let title = format!(
        "{kind}: {} {} - {}",
        manifest("name"),
        manifest("version"),
        start.run_name
    );
    let success = ending.is_some_and(|ending| ending.success);
    let color = match card {
        Card::Start => None,
        Card::Complete if !success => Some("attention"),
        Card::Error => Some("attention"),
        _ => Some("good"),
    };
    let end = ending.map_or(String::from("null"), |ending| offset_time(ending.end));
    let duration = ending.map_or(String::from("null"), |ending| {
        nextflow_evaluator::units::duration_text(ending.end - start.start)
    });
    let error = ending
        .and_then(|ending| ending.error_message.clone())
        .unwrap_or_else(|| "null".to_owned());
    let mut body = vec![title_block(&title, color)];
    let subtitle = match card {
        Card::Start => format!("Started on {}", offset_time(start.start)),
        Card::Error => format!("Failed on {end} ({duration})"),
        _ => format!("Completed on {end} ({duration})"),
    };
    body.push(text_block(
        &[("spacing", "None"), ("text", &subtitle)],
        true,
    ));
    let failed = "The pipeline failed because of the following issue:";
    match card {
        Card::Complete if success => {
            body.push(text_block(
                &[("text", "The pipeline completed successfully")],
                false,
            ));
        }
        Card::Complete | Card::Error => {
            body.push(text_block(&[("text", failed)], false));
            body.push(text_block(
                &[("color", "attention"), ("text", &error)],
                false,
            ));
        }
        _ => {}
    }
    let launch = "The command used to launch the workflow was as follows:";
    body.push(text_block(&[("text", launch)], false));
    body.push(text_block(&[("text", &start.command_line)], true));
    let text = |value: Option<&String>| value.cloned().unwrap_or_else(|| "null".to_owned());
    let mut facts = vec![("Pipeline start", offset_time(start.start))];
    if card != Card::Start {
        facts.push(("Pipeline end", end));
    }
    facts.extend([
        ("Script file", start.script.clone()),
        ("Script ID", text(run.script_id.as_ref())),
        ("Repository", text(start.repository.as_ref())),
        ("Revision", text(start.revision.as_ref())),
        ("Commit ID", text(start.commit_id.as_ref())),
        ("User ID", std::env::var("USER").unwrap_or_default()),
        (
            "Nextflow version",
            crate::report_format::NEXTFLOW_VERSION.to_owned(),
        ),
        ("Nextflow build", NEXTFLOW_BUILD.to_string()),
        ("Nextflow timestamp", build_time()),
    ]);
    let facts = facts.into_iter().map(|(title, value)| {
        Json::object([("title", Json::from(title)), ("value", Json::from(value))])
    });
    let fact_set = Json::object([
        ("type", Json::from("FactSet")),
        ("facts", Json::List(facts.collect())),
    ]);
    let action = Json::object([
        ("type", Json::from("Action.ShowCard")),
        ("title", Json::from("Pipeline Configuration")),
        (
            "card",
            Json::object([
                ("type", Json::from("AdaptiveCard")),
                ("$schema", Json::from(SCHEMA)),
                ("body", Json::list([fact_set])),
            ]),
        ),
    ]);
    let content = Json::object([
        ("$schema", Json::from(SCHEMA)),
        (
            "msteams",
            Json::object([
                ("width", Json::from("Full")),
                ("summary", Json::from(&title)),
            ]),
        ),
        ("type", Json::from("AdaptiveCard")),
        ("version", Json::from("1.2")),
        ("body", Json::List(body)),
        ("actions", Json::list([action])),
    ]);
    let attachment = Json::object([
        (
            "contentType",
            Json::from("application/vnd.microsoft.card.adaptive"),
        ),
        ("contentUrl", Json::Null),
        ("content", content),
    ]);
    Json::object([
        ("type", Json::from("message")),
        ("attachments", Json::list([attachment])),
    ])
}

fn title_block(title: &str, color: Option<&str>) -> Json {
    let mut entries = vec![
        ("type", Json::from("TextBlock")),
        ("size", Json::from("Large")),
        ("weight", Json::from("Bolder")),
    ];
    if let Some(color) = color {
        entries.push(("color", Json::from(color)));
    }
    entries.extend([("text", Json::from(title)), ("wrap", Json::from(true))]);
    Json::object(entries)
}

/// A text block with these fields after its type, then `isSubtle` when `subtle`.
fn text_block(fields: &[(&str, &str)], subtle: bool) -> Json {
    let mut entries = vec![("type", Json::from("TextBlock"))];
    entries.extend(fields.iter().map(|(key, value)| (*key, Json::from(*value))));
    if subtle {
        entries.push(("isSubtle", Json::from(true)));
    }
    entries.push(("wrap", Json::from(true)));
    Json::object(entries)
}

/// The build time of Nextflow as the cards show it, like `09-07-2026 18:49 UTC`.
fn build_time() -> String {
    let (date, time) = NEXTFLOW_TIMESTAMP.split_once('T').unwrap_or_default();
    let parts: Vec<&str> = date.split('-').collect();
    match parts.as_slice() {
        [year, month, day] => format!("{day}-{month}-{year} {} UTC", time.get(..5).unwrap_or("")),
        _ => NEXTFLOW_TIMESTAMP.to_owned(),
    }
}

/// Renders a template file as Groovy's `GStringTemplateEngine` does, as far as uwe's
/// evaluator goes: `session` has `workflowMetadata`, `manifest` and `params`, and the error
/// card's `event` has the failing task's `trace`.
fn render(template: &str, run: &Run, end: Option<&End>) -> Result<String, String> {
    let path = Path::new(&run.start.launch_dir).join(template);
    let text = std::fs::read_to_string(&path).map_err(|error| error.to_string())?;
    let workflow = run.workflow(|millis| Json::from(offset_time(millis)));
    let session = Value::map([
        (
            Value::string("workflowMetadata"),
            script_value(&json(&workflow)),
        ),
        (Value::string("manifest"), script_value(&run.start.manifest)),
        (Value::string("params"), script_value(&run.start.params)),
    ]);
    let mut binding = vec![(Rc::from("session"), Rc::new(RefCell::new(session)))];
    if let Some(task) = end.and_then(|end| end.task.as_ref()) {
        let trace = script_value(&serde_json::Value::Object(task.record.clone()));
        let event = Value::map([(Value::string("trace"), trace)]);
        binding.push((Rc::from("event"), Rc::new(RefCell::new(event))));
    }
    let mut host = Quiet;
    let mut interpreter = Interpreter::new(&mut host);
    (interpreter.render_template(&text, binding, Span::default()))
        .map_err(|error| error.to_string())
}

/// A host that templates cannot print to.
struct Quiet;

impl Host for Quiet {
    fn print(&mut self, _: &str) {}
}

fn json(value: &Json) -> serde_json::Value {
    serde_json::from_str(&value.compact()).unwrap_or_default()
}

/// A JSON value as the script value of the same shape.
fn script_value(value: &serde_json::Value) -> Value {
    match value {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(value) => Value::Boolean(*value),
        serde_json::Value::Number(number) => match number.as_i64() {
            Some(whole) => match i32::try_from(whole) {
                Ok(small) => Value::integer(small),
                Err(_) => Value::Number(Number::Long(whole)),
            },
            None => Value::Number(Number::Double(number.as_f64().unwrap_or_default())),
        },
        serde_json::Value::String(text) => Value::string(text.as_str()),
        serde_json::Value::Array(items) => Value::list(items.iter().map(script_value).collect()),
        serde_json::Value::Object(entries) => Value::map(
            (entries.iter()).map(|(key, value)| (Value::string(key.as_str()), script_value(value))),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_build_time() {
        assert_eq!(build_time(), "09-07-2026 18:49 UTC");
    }
}
