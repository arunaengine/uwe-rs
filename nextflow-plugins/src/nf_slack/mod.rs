//! nf-slack 0.5.1: Slack messages at the start and end of a run, its progress and reactions,
//! file uploads, and the script functions `slackMessage` and `slackFileUpload`.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod api;
mod blocks;
mod settings;
mod worker;

use std::cell::RefCell;
use std::time::Duration;

use nextflow_config::Config;
use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::value::Value;

use crate::Context;
use crate::catalogue::{Kind, Plugin, State};
use crate::observer::{Background, Event, Observer};
use crate::report_format::value_json;
use api::Upload;
use settings::{Mode, Settings};
use worker::{Queue, Script, Worker};

pub(crate) const PLUGIN: Plugin = Plugin {
    functions: &["slackMessage", "slackFileUpload"],
    call: Some(call),
    scopes: &["slack"],
    observer: Some(activate),
    ..Plugin::new("nf-slack", Kind::Observer, State::Implemented)
};

/// How often the worker looks for script messages and due progress updates.
const TICK: Duration = Duration::from_millis(100);
/// How long the end of the run waits for the messages that are left.
const WAIT: Duration = Duration::from_secs(60);

thread_local! {
    /// The queue of the run on this thread, which the script functions add to; a run's
    /// script and its observers share the driver's thread.
    static QUEUE: RefCell<Option<Queue>> = const { RefCell::new(None) };
}

fn activate(config: &Config) -> Result<Option<Box<dyn Observer>>, String> {
    QUEUE.with(|slot| slot.borrow_mut().take());
    let Some(settings) = Settings::read(config) else {
        return Ok(None);
    };
    match settings.mode {
        Mode::Bot { .. } => tracing::info!("Slack plugin: Enabled with Bot notifications"),
        Mode::Webhook { .. } => tracing::info!("Slack plugin: Enabled with Webhook notifications"),
    }
    let queue = Queue::default();
    QUEUE.with(|slot| *slot.borrow_mut() = Some(queue.clone()));
    let mut worker = Worker::new(settings, queue.clone());
    let background = Background::ticking("nf-slack", WAIT, TICK, move |event| {
        worker.handle(event);
    })
    .map_err(|error| error.to_string())?;
    Ok(Some(Box::new(Slack { background, queue })))
}

/// The run observer, which marks the start of the run among the requests of the script.
struct Slack {
    background: Background,
    queue: Queue,
}

impl Observer for Slack {
    fn notify(&mut self, event: &Event) {
        if let (Event::RunStarted(_), Ok(mut queue)) = (event, self.queue.lock()) {
            queue.push(Script::Begun);
        }
        self.background.notify(event);
    }
}

/// `slackMessage` and `slackFileUpload`: they hand their request to the run's worker, which
/// sends it after the start message; they never fail and do nothing without the plugin.
fn call(name: &str, arguments: Vec<Value>, _: &mut Context) -> Result<Value, Error> {
    let Some(argument) = arguments.first() else {
        return Ok(Value::Null);
    };
    let options = match argument {
        Value::Map(_) => Some(value_json(argument)),
        _ => None,
    };
    let text = |key: &str| {
        let value = &options.as_ref()?[key];
        (!value.is_null()).then(|| plain(value))
    };
    let script = match (name, &options) {
        ("slackMessage", None) => Script::Message(blocks::text_message(&argument.to_string())),
        ("slackMessage", Some(map)) => {
            let Some(message) = text("message") else {
                tracing::error!("Slack plugin: 'message' parameter is required for rich messages");
                return Ok(Value::Null);
            };
            let fields: Vec<(String, String)> = (map["fields"].as_array().into_iter().flatten())
                .map(|field| (plain(&field["title"]), plain(&field["value"])))
                .collect();
            Script::Message(blocks::rich_message(&message, &fields))
        }
        ("slackFileUpload", None) => Script::Upload(Upload {
            file: argument.to_string(),
            title: None,
            comment: None,
            filename: None,
        }),
        ("slackFileUpload", Some(_)) => {
            let Some(file) = text("file") else {
                tracing::error!("Slack plugin: 'file' parameter is required for file upload");
                return Ok(Value::Null);
            };
            Script::Upload(Upload {
                file,
                title: text("title"),
                comment: text("comment"),
                filename: text("filename"),
            })
        }
        _ => return Ok(Value::Null),
    };
    QUEUE.with(|slot| {
        if let Some(queue) = slot.borrow().as_ref()
            && let Ok(mut queue) = queue.lock()
        {
            queue.push(script);
        }
    });
    Ok(Value::Null)
}

fn plain(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}
