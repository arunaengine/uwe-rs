//! Requests to the Slack Web API and to webhooks, and Slack's three steps of a file upload.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;
use std::time::Duration;

use reqwest::Method;

use crate::report_format::Json;
use crate::service::{self, Sender, form_encode};

/// How long one request may take.
const LIMIT: Duration = Duration::from_secs(30);
/// The largest file that the plugin uploads.
const LARGEST: u64 = 100 * 1024 * 1024;

/// A file that a script or the config uploads.
pub(super) struct Upload {
    pub(super) file: String,
    pub(super) title: Option<String>,
    pub(super) comment: Option<String>,
    pub(super) filename: Option<String>,
}

/// Where an upload goes: the bot's API, its channel and the thread of the start message.
pub(super) struct Target<'a> {
    pub(super) api: &'a str,
    pub(super) token: &'a str,
    pub(super) channel: &'a str,
    pub(super) thread: Option<String>,
}

/// Why a request failed.
pub(super) enum Failure {
    Status(u16, String),
    Api(String),
    Request(String),
}

/// Sends the requests of one run.
#[derive(Default)]
pub(super) struct Client {
    sender: Option<Sender>,
}

impl Client {
    /// Uploads a file in Slack's three steps: an upload address, the bytes, then the file's
    /// place in the channel.
    pub(super) fn upload(
        &mut self,
        path: &Path,
        upload: &Upload,
        target: &Target,
    ) -> Result<(), String> {
        let Target {
            api,
            token,
            channel,
            ..
        } = *target;
        let shown = path.display().to_string();
        let metadata = std::fs::metadata(path)
            .map_err(|_| format!("Slack plugin: File not found: {shown}"))?;
        if metadata.len() == 0 {
            return Err(format!("Slack plugin: Cannot upload empty file: {shown}"));
        }
        if metadata.len() > LARGEST {
            return Err(format!(
                "Slack plugin: File exceeds maximum size of 100MB: {shown}"
            ));
        }
        let bytes = std::fs::read(path)
            .map_err(|_| format!("Slack plugin: File is not readable: {shown}"))?;
        let name = (upload.filename.clone())
            .or_else(|| Some(path.file_name()?.to_string_lossy().into_owned()))
            .unwrap_or_default();
        let url = format!(
            "{api}files.getUploadURLExternal?filename={}&length={}",
            form_encode(&name),
            bytes.len()
        );
        let address = self.answer(Method::GET, &url, token, None);
        let address = address.map_err(|failure| match failure {
            Failure::Status(code, text) => {
                format!("Slack plugin: Failed to get upload URL - HTTP {code}: {text}")
            }
            Failure::Api(error) | Failure::Request(error) => {
                format!("Slack plugin: Failed to get upload URL - API error: {error}")
            }
        })?;
        let (Some(address), Some(id)) =
            (address["upload_url"].as_str(), address["file_id"].as_str())
        else {
            return Err("Slack plugin: Failed to get upload URL - API error: no upload URL".into());
        };
        let sender = service::sender(&mut self.sender)?;
        let request = (sender.client().post(address))
            .timeout(LIMIT)
            .header("Content-Type", "application/octet-stream")
            .body(bytes);
        match sender.send(request) {
            Ok(answer) if answer.status == 200 => {}
            Ok(answer) => {
                return Err(format!(
                    "Slack plugin: Failed to upload file content - HTTP {}: {}",
                    answer.status, answer.body
                ));
            }
            Err(error) => {
                let error = error.without_url();
                return Err(format!(
                    "Slack plugin: Failed to upload file content - {error}"
                ));
            }
        }
        let title = upload.title.clone().unwrap_or_else(|| name.clone());
        let file = Json::object([("id", Json::from(id)), ("title", Json::from(title))]);
        let mut entries = vec![
            ("files", Json::list([file])),
            ("channel_id", Json::from(channel)),
        ];
        if let Some(comment) = &upload.comment {
            entries.push(("initial_comment", Json::from(comment)));
        }
        if let Some(ts) = &target.thread {
            entries.push(("thread_ts", Json::from(ts)));
        }
        let url = format!("{api}files.completeUploadExternal");
        match self.call(&url, token, &Json::object(entries)) {
            Ok(_) => Ok(()),
            Err(Failure::Status(code, text)) => Err(format!(
                "Slack plugin: Failed to complete file upload - HTTP {code}: {text}"
            )),
            Err(Failure::Api(error) | Failure::Request(error)) => Err(format!(
                "Slack plugin: Failed to complete file upload - API error: {error}"
            )),
        }
    }

    /// A POST of the Web API with a JSON body.
    pub(super) fn call(
        &mut self,
        url: &str,
        token: &str,
        body: &Json,
    ) -> Result<serde_json::Value, Failure> {
        self.answer(Method::POST, url, token, Some(body.compact()))
    }

    /// A Web API request, which succeeds when HTTP and Slack say so.
    pub(super) fn answer(
        &mut self,
        method: Method,
        url: &str,
        token: &str,
        body: Option<String>,
    ) -> Result<serde_json::Value, Failure> {
        let (code, text) = self
            .send(method, url, Some(token), body)
            .map_err(Failure::Request)?;
        if code != 200 {
            return Err(Failure::Status(code, text));
        }
        let answer: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
        match answer["ok"].as_bool() {
            Some(true) => Ok(answer),
            _ => Err(Failure::Api(
                answer["error"]
                    .as_str()
                    .unwrap_or("unknown error")
                    .to_owned(),
            )),
        }
    }

    /// Sends one request; the error leaves out the address, which may hold a secret.
    pub(super) fn send(
        &mut self,
        method: Method,
        url: &str,
        token: Option<&str>,
        body: Option<String>,
    ) -> Result<(u16, String), String> {
        let sender = service::sender(&mut self.sender)?;
        let mut request = sender.client().request(method, url).timeout(LIMIT);
        if let Some(token) = token {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        if let Some(body) = body {
            let kind = match token {
                Some(_) => "application/json; charset=utf-8",
                None => "application/json",
            };
            request = request.header("Content-Type", kind).body(body);
        }
        match sender.send(request) {
            Ok(answer) => Ok((answer.status, answer.body)),
            Err(error) => Err(error.without_url().to_string()),
        }
    }

    /// Checks the token, then asks Slack whether it works.
    pub(super) fn validate(&mut self, api: &str, token: &str) {
        let url = format!("{api}auth.test");
        let problem = if !token.starts_with("xoxb-") && !token.starts_with("xoxp-") {
            tracing::warn!("Slack plugin: Bot token must start with 'xoxb-' or 'xoxp-'");
            None
        } else {
            if token.starts_with("xoxp-") {
                tracing::warn!(
                    "Slack plugin: You are using a User Token (xoxp-). It is recommended to use \
                     a Bot Token (xoxb-) for better security and granular permissions."
                );
            }
            match self.call(&url, token, &Json::object::<&str>([])) {
                Ok(_) => return,
                Err(Failure::Status(code, _)) => Some(format!("HTTP {code}")),
                Err(Failure::Api(error) | Failure::Request(error)) => Some(error),
            }
        };
        if let Some(problem) = problem {
            tracing::warn!("Slack plugin: Connection validation failed - {problem}");
        }
        tracing::warn!(
            "Slack plugin: Connection validation failed - Slack notifications may not work. Set \
             slack.validateOnStartup = false to skip validation."
        );
    }
}
