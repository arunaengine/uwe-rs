//! The worker thread of nf-slack: posts the messages in order, keeps the progress update and
//! the reactions of the start message, and uploads files.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use reqwest::Method;

use super::api::{Client, Failure, Target, Upload};
use super::blocks::{self, Counts, Facts, Kind};
use super::settings::{Mode, Settings};
use crate::observer::{End, Event, Start};
use crate::report_format::{Json, now};

/// What a script asks for with `slackMessage` and `slackFileUpload`.
pub(super) enum Script {
    Message(Vec<Json>),
    Upload(Upload),
    /// The run started: what follows came from handlers like `workflow.onComplete`.
    Begun,
}

/// The requests of the scripts, which the worker sends after the start message.
pub(super) type Queue = Arc<Mutex<Vec<Script>>>;

pub(super) struct Worker {
    settings: Settings,
    queue: Queue,
    client: Client,
    start: Option<Start>,
    /// The time stamp of the start message and the channel id that Slack gave for it.
    ts: Option<String>,
    channel: Option<String>,
    counts: Counts,
    last_update: Option<Instant>,
    /// When the progress update that waits for its interval is due.
    update_due: Option<Instant>,
    reacted: bool,
    /// The errors already shown, for those shown once.
    shown: HashSet<String>,
}

impl Worker {
    pub(super) fn new(settings: Settings, queue: Queue) -> Worker {
        Worker {
            settings,
            queue,
            client: Client::default(),
            start: None,
            ts: None,
            channel: None,
            counts: Counts {
                submitted: 0,
                completed: 0,
                cached: 0,
                failed: 0,
            },
            last_update: None,
            update_due: None,
            reacted: false,
            shown: HashSet::new(),
        }
    }

    pub(super) fn handle(&mut self, event: Option<Event>) {
        match &event {
            Some(Event::RunStarted(start)) => self.started(start),
            Some(Event::TaskSubmitted(_)) => self.count(|counts| counts.submitted += 1),
            Some(Event::TaskCompleted(_) | Event::TaskFailed(_)) => {
                self.count(|counts| counts.completed += 1);
            }
            Some(Event::TaskCached(_)) => self.count(|counts| counts.cached += 1),
            Some(Event::RunEnded(end)) => self.ended(end),
            _ => {}
        }
        if self.start.is_some() {
            self.drain(false);
        }
        if self.update_due.is_some_and(|due| Instant::now() >= due) {
            self.update();
        }
    }

    /// Validates the connection, posts the start message with its progress, sends what the
    /// script asked for, then marks the start message.
    fn started(&mut self, start: &Start) {
        self.start = Some(start.clone());
        match &self.settings.mode {
            Mode::Bot { token, api, .. } if self.settings.validate => {
                let (token, api) = (token.clone(), api.clone());
                self.client.validate(&api, &token);
            }
            Mode::Webhook { .. } if self.settings.validate => tracing::info!(
                "Slack plugin: Webhook connections have limited validation - token and channel \
                 checks are not available"
            ),
            _ => {}
        }
        if self.settings.start.enabled {
            let facts = Facts::of(self.start.as_ref(), None);
            let blocks = blocks::notice(Kind::Start, &self.settings.start, &facts, true);
            self.post(blocks, false);
        }
        if self.settings.progress.is_some() {
            self.update();
        }
        self.drain(true);
        if self.settings.start.enabled
            && let Some(reaction) = self.settings.reactions.as_ref().map(|r| r.start.clone())
        {
            self.reacted = self.react("add", &reaction);
        }
    }

    /// The end of the run: what the script asked for, the last progress, then the success or
    /// failure message with its files and reaction. uwe sends the failure message, which
    /// Nextflow 26.04.6 never sends.
    fn ended(&mut self, end: &End) {
        self.drain(false);
        self.counts.completed = end.counts.succeeded;
        self.counts.cached = end.counts.cached;
        self.counts.failed = end.counts.failed;
        if self.settings.progress.is_some() {
            self.update();
        }
        let (kind, notice) = match end.success {
            true => (Kind::Success, &self.settings.complete),
            false => (Kind::Failure, &self.settings.error),
        };
        if notice.enabled {
            let blocks = blocks::notice(
                kind,
                notice,
                &Facts::of(self.start.as_ref(), Some(end)),
                true,
            );
            let files = notice.files.clone();
            self.post(blocks, true);
            for file in files {
                let upload = Upload {
                    file,
                    title: None,
                    comment: None,
                    filename: None,
                };
                if let Err(message) = self.upload(&upload, true) {
                    tracing::warn!(
                        "Slack plugin: Failed to upload file {}: {message}",
                        upload.file
                    );
                }
            }
        }
        let Some(reactions) = &self.settings.reactions else {
            return;
        };
        let (start, after) = match end.success {
            true => (reactions.start.clone(), reactions.success.clone()),
            false => (reactions.start.clone(), reactions.error.clone()),
        };
        if self.reacted {
            self.react("remove", &start);
        }
        self.react("add", &after);
    }

    fn count(&mut self, change: impl FnOnce(&mut Counts)) {
        change(&mut self.counts);
        let Some(interval) = self.settings.progress else {
            return;
        };
        match self.last_update {
            Some(last) if last.elapsed() < interval => {
                self.update_due.get_or_insert(last + interval);
            }
            _ => self.update(),
        }
    }

    /// Sends the messages and uploads that the script asked for, in order; at the start only
    /// those of the script's body, as the handlers of the run's end follow later.
    fn drain(&mut self, body: bool) {
        let scripts = match self.queue.lock() {
            Ok(mut queue) => {
                let begun = queue
                    .iter()
                    .position(|script| matches!(script, Script::Begun));
                match begun.filter(|_| body) {
                    Some(place) => {
                        let rest = queue.split_off(place + 1);
                        std::mem::replace(&mut *queue, rest)
                    }
                    None => std::mem::take(&mut *queue),
                }
            }
            Err(_) => return,
        };
        for script in scripts {
            match script {
                Script::Begun => {}
                Script::Message(blocks) => self.post(blocks, true),
                Script::Upload(upload) => match &self.settings.mode {
                    Mode::Webhook { .. } => tracing::warn!(
                        "Slack plugin: File upload is not supported with webhooks. Please \
                         configure a bot token to upload files."
                    ),
                    Mode::Bot { .. } => {
                        if let Err(message) = self.upload(&upload, false) {
                            tracing::error!("{message}");
                        }
                    }
                },
            }
        }
    }

    /// Posts a message; `thread` answers the start message in its thread.
    fn post(&mut self, blocks: Vec<Json>, thread: bool) {
        let (url, token, channel) = match &self.settings.mode {
            Mode::Webhook { url } => (url.clone(), None, None),
            Mode::Bot {
                token,
                channel,
                api,
            } => (
                format!("{api}chat.postMessage"),
                Some(token.clone()),
                Some(channel.clone()),
            ),
        };
        let mut entries = vec![("blocks", Json::List(blocks))];
        if let Some(channel) = &channel {
            entries.push(("channel", Json::from(channel)));
        }
        if let Some(ts) = self.ts.clone().filter(|_| thread && self.settings.threads) {
            entries.push(("thread_ts", Json::from(ts)));
        }
        let body = Json::object(entries);
        let Some(token) = token else {
            let failure = match self
                .client
                .send(Method::POST, &url, None, Some(body.compact()))
            {
                Ok((200, _)) => return,
                Ok((code, text)) => format!("Slack webhook HTTP {code}: {text}"),
                Err(message) => format!("Slack plugin: Error sending message: {message}"),
            };
            return self.show_once(failure);
        };
        match self.client.call(&url, &token, &body) {
            Ok(answer) => {
                if self.ts.is_none() {
                    self.ts = answer["ts"].as_str().map(str::to_owned);
                    self.channel = answer["channel"].as_str().map(str::to_owned);
                }
            }
            Err(Failure::Status(code, text)) => {
                self.show_once(format!("Slack plugin: HTTP {code}: {text}"));
            }
            Err(Failure::Api(error)) => self.show_once(format!("Slack plugin: API error: {error}")),
            Err(Failure::Request(message)) => {
                self.show_once(format!(
                    "Slack plugin: Error sending bot message: {message}"
                ));
            }
        }
    }

    /// Replaces the start message with its progress table.
    fn update(&mut self) {
        self.update_due = None;
        let (
            Mode::Bot {
                token,
                channel,
                api,
            },
            Some(ts),
        ) = (&self.settings.mode, &self.ts)
        else {
            return;
        };
        let begun = self.start.as_ref().map_or(0, |start| start.start);
        let elapsed = u64::try_from((now() - begun) / 1000).unwrap_or(0);
        let facts = Facts::of(self.start.as_ref(), None);
        let start = blocks::notice(Kind::Start, &self.settings.start, &facts, false);
        let mut entries = vec![
            (
                "blocks",
                Json::List(blocks::progress(start, &self.counts, elapsed)),
            ),
            (
                "channel",
                Json::from(self.channel.clone().unwrap_or_else(|| channel.clone())),
            ),
        ];
        if self.settings.threads {
            entries.push(("thread_ts", Json::from(ts)));
        }
        entries.push(("ts", Json::from(ts)));
        let (url, token) = (format!("{api}chat.update"), token.clone());
        self.last_update = Some(Instant::now());
        let message = match self.client.call(&url, &token, &Json::object(entries)) {
            Ok(_) => return,
            Err(Failure::Status(code, text)) => format!("- HTTP {code}: {text}"),
            Err(Failure::Api(error)) => format!("- API error: {error}"),
            Err(Failure::Request(message)) => {
                return tracing::error!("Slack plugin: Error updating message: {message}");
            }
        };
        tracing::error!("Slack plugin: Failed to update message {message}");
    }

    /// Adds or removes a reaction of the start message; true when Slack took it.
    fn react(&mut self, action: &str, emoji: &str) -> bool {
        let (
            Mode::Bot {
                token,
                channel,
                api,
            },
            Some(ts),
        ) = (&self.settings.mode, &self.ts)
        else {
            return false;
        };
        let body = Json::object([
            (
                "channel",
                Json::from(self.channel.clone().unwrap_or_else(|| channel.clone())),
            ),
            ("name", Json::from(emoji)),
            ("timestamp", Json::from(ts)),
        ]);
        let (url, token) = (format!("{api}reactions.{action}"), token.clone());
        let error = match self.client.call(&url, &token, &body) {
            Ok(_) => return true,
            Err(Failure::Api(error)) if error == "no_reaction" || error == "already_reacted" => {
                return true;
            }
            Err(Failure::Api(error)) if error == "missing_scope" => {
                format!("{error} (add reactions:write scope to your Slack app)")
            }
            Err(Failure::Api(error) | Failure::Request(error)) => error,
            Err(Failure::Status(code, text)) => format!("HTTP {code}: {text}"),
        };
        tracing::warn!("Slack plugin: Failed to {action} reaction '{emoji}': {error}");
        false
    }

    /// Uploads a file of a script or of the config, whose relative paths start in the launch
    /// directory.
    fn upload(&mut self, upload: &Upload, from_config: bool) -> Result<(), String> {
        let Mode::Bot {
            token,
            channel,
            api,
        } = &self.settings.mode
        else {
            return Ok(());
        };
        let mut path = Path::new(&upload.file).to_owned();
        if let Some(start) = self
            .start
            .as_ref()
            .filter(|_| from_config && path.is_relative())
        {
            path = Path::new(&start.launch_dir).join(&path);
        }
        let target = Target {
            api,
            token,
            channel,
            thread: self.ts.clone().filter(|_| self.settings.threads),
        };
        self.client.upload(&path, upload, &target)
    }

    fn show_once(&mut self, message: String) {
        if self.shown.insert(message.clone()) {
            tracing::error!("{message}");
        }
    }
}
