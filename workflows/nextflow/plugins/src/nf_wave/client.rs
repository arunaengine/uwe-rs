//! Sends the requests of nf-wave to the Wave service and waits for the images it builds.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::time::{Duration, Instant};

use serde_json::Value as Json;

use super::Wave;
use crate::service::{random_hex, random_unit, safe_url};

impl Wave {
    /// Sends the request, again after network errors and answers 429 and 5xx.
    pub(super) fn submit(&mut self, request: &Json) -> Result<Json, String> {
        let url = format!("{}/v1alpha2/container", self.settings.endpoint);
        let body = request.to_string();
        let mut pause = self.settings.retry.delay;
        let attempts = self.settings.retry.attempts.max(1);
        for attempt in 1..=attempts {
            self.space();
            let token = self.settings.tower.token.clone();
            let traceparent = format!("00-{}-{}-01", random_hex(32), random_hex(16));
            let answer = self.send(|client| {
                let mut request = (client.post(&url))
                    .header("Content-Type", "application/json")
                    .header("Traceparent", traceparent)
                    .body(body.clone());
                if let Some(token) = token {
                    request = request.bearer_auth(token);
                }
                request
            });
            let retried = match &answer {
                Ok((status, _)) => *status == 429 || *status >= 500,
                Err(_) => true,
            };
            if retried && attempt < attempts {
                let jitter = 1.0 + self.settings.retry.jitter * (2.0 * random_unit() - 1.0);
                std::thread::sleep(pause.mul_f64(jitter.max(0.0)));
                pause = pause
                    .mul_f64(self.settings.retry.factor)
                    .min(self.settings.retry.longest);
                continue;
            }
            return match answer {
                Ok((200, text)) => serde_json::from_str(&text).map_err(|error| {
                    format!(
                        "Wave invalid response: POST {} [200] {error}",
                        safe_url(&url)
                    )
                }),
                Ok((status, text)) => Err(format!(
                    "Wave invalid response: POST {} [{status}] {text}",
                    safe_url(&url)
                )),
                Err(error) => Err(format!(
                    "Unable to connect Wave service: {} - cause: {error}",
                    safe_url(&self.settings.endpoint)
                )),
            };
        }
        Err(format!(
            "Unable to connect Wave service: {}",
            safe_url(&self.settings.endpoint)
        ))
    }

    /// Waits until the image of an answer is ready: at once without a build, else until its
    /// status says it is done.
    pub(super) fn ready(&mut self, answer: &Json, target: &str) -> Result<(), String> {
        let text = |key: &str| answer[key].as_str().map(str::to_owned);
        if answer["succeeded"] == Json::Bool(true) || answer["cached"] == Json::Bool(true) {
            return Ok(());
        }
        let endpoint = self.settings.endpoint.clone();
        let (status_url, view, done) = match (text("requestId"), text("buildId")) {
            (Some(_), None) if answer["mirror"] != Json::Bool(true) => return Ok(()),
            (Some(id), _) => (
                format!("{endpoint}/v1alpha2/container/{id}/status"),
                format!("{endpoint}/view/containers/{id}"),
                "DONE",
            ),
            (None, Some(id)) => (
                format!("{endpoint}/v1alpha1/builds/{id}/status"),
                format!("{endpoint}/view/builds/{id}"),
                "COMPLETED",
            ),
            (None, None) => return Ok(()),
        };
        let started = Instant::now();
        let mut told = Duration::from_secs(10);
        loop {
            let (status, body) = self.send(|client| client.get(&status_url))?;
            let state: Json = match status {
                200 => serde_json::from_str(&body).map_err(|error| error.to_string())?,
                _ => {
                    return Err(format!(
                        "Wave invalid response: GET {} [{status}] {body}",
                        safe_url(&status_url)
                    ));
                }
            };
            if state["status"].as_str() == Some(done) {
                if state["succeeded"] == Json::Bool(true) {
                    return Ok(());
                }
                let mut message = format!(
                    "Wave provisioning for container '{target}' did not complete successfully"
                );
                if done == "COMPLETED" {
                    message.push_str(&format!(" - check details here: {}", safe_url(&view)));
                }
                if let Some(reason) = state["reason"].as_str() {
                    message.push_str(&format!("\n- Reason: {reason}"));
                }
                if let Some(details) = state["detailsUri"].as_str() {
                    message.push_str(&format!("\n- Find out more here: {details}"));
                }
                return Err(message);
            }
            let waited = started.elapsed();
            if waited > self.settings.build_limit {
                return Err(format!(
                    "Wave provisioning for container '{target}' is exceeding max allowed duration ({}) - check details here: {}",
                    shown(self.settings.build_limit),
                    safe_url(&view)
                ));
            }
            if waited >= told {
                tracing::info!("Awaiting container provisioning: {target}");
                told += Duration::from_secs(200);
            }
            std::thread::sleep(self.poll);
        }
    }

    /// Keeps the requests of the run apart by the pause of `httpClient.maxRate`.
    fn space(&mut self) {
        if let Some(sent) = self.sent {
            let waited = sent.elapsed();
            if waited < self.settings.spacing {
                std::thread::sleep(self.settings.spacing - waited);
            }
        }
        self.sent = Some(Instant::now());
    }

    /// Sends one request with the HTTP client of the process and reads the whole answer.
    fn send(
        &mut self,
        build: impl FnOnce(&reqwest::Client) -> reqwest::RequestBuilder,
    ) -> Result<(u16, String), String> {
        let client = storage::http_client()?;
        if self.runtime.is_none() {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| error.to_string())?;
            self.runtime = Some(runtime);
        }
        let runtime = self.runtime.as_ref().ok_or("no runtime")?;
        let request = build(client).timeout(Duration::from_secs(60));
        runtime.block_on(async {
            let response = request.send().await.map_err(causes)?;
            let status = response.status().as_u16();
            let text = response.text().await.map_err(causes)?;
            Ok((status, text))
        })
    }
}

/// An error with up to three of its causes, joined by ` <- `, without the request address.
fn causes(error: reqwest::Error) -> String {
    let error = error.without_url();
    let mut parts = vec![error.to_string()];
    let mut source = std::error::Error::source(&error);
    while let Some(cause) = source.filter(|_| parts.len() < 3) {
        parts.push(cause.to_string());
        source = cause.source();
    }
    parts.join(" <- ")
}

/// A duration as Nextflow prints it, like `40m` or `90s`.
fn shown(duration: Duration) -> String {
    let seconds = duration.as_secs();
    match seconds {
        0 => format!("{}ms", duration.as_millis()),
        _ if seconds % 3600 == 0 => format!("{}h", seconds / 3600),
        _ if seconds % 60 == 0 => format!("{}m", seconds / 60),
        _ => format!("{seconds}s"),
    }
}
