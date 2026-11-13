//! Requests to the Seqera Platform API: authentication, retries and the messages of failures.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use reqwest::Method;

use crate::report_format::{Json, NEXTFLOW_VERSION};
use crate::service::{Answer, Sender, form_encode, random_hex, random_unit, safe_url};

/// Answers that are tried again; others fail at once.
const RETRIED: [u16; 5] = [429, 500, 502, 503, 504];

/// The API of one Platform, with the credentials of the run.
pub(super) struct Api {
    pub(super) endpoint: String,
    pub(super) workspace: Option<String>,
    pub(super) token: String,
    /// Refreshes a JWT token after an answer of 401.
    pub(super) refresh: Option<String>,
    /// How long one request may take.
    pub(super) timeout: Duration,
    pub(super) retry: Retry,
}

/// `tower.retryPolicy`.
pub(super) struct Retry {
    pub(super) delay: Duration,
    pub(super) max_delay: Duration,
    /// Attempts in all, the first one included.
    pub(super) attempts: u32,
    pub(super) jitter: f64,
    pub(super) multiplier: f64,
}

impl Retry {
    /// The wait before attempt `attempt + 1`, grown by `multiplier` and spread by `jitter`.
    fn delay(&self, attempt: u32) -> Duration {
        let grown = self.delay.as_secs_f64() * self.multiplier.powi(attempt as i32 - 1);
        let bounded = grown.min(self.max_delay.as_secs_f64());
        let spread = 1.0 + self.jitter * (2.0 * random_unit() - 1.0);
        Duration::from_secs_f64((bounded * spread).max(0.0))
    }
}

/// A request that failed, as the plugin reports it.
#[derive(Debug)]
pub(super) struct Problem {
    pub(super) url: String,
    /// The status of the answer, 0 when there was none.
    pub(super) code: u16,
    pub(super) message: String,
}

impl Problem {
    /// The message of a failed create, begin or progress request.
    pub(super) fn report(&self) -> String {
        format!(
            "Unexpected HTTP response\n- endpoint    : {}\n- status code : {}\n- response msg: {}",
            safe_url(&self.url),
            self.code,
            self.message
        )
    }

    /// The warning of a failed heartbeat or complete request.
    pub(super) fn warning(&self, endpoint: &str) -> String {
        format!(
            "Unexpected HTTP response.\nFailed to send message to {endpoint} -- received\n\
             - status code : {}\n- response msg: {}\n- error cause : -",
            self.code,
            self.message,
            endpoint = safe_url(endpoint)
        )
    }

    fn of_answer(url: &str, answer: &Answer) -> Problem {
        let message = match answer.status {
            401 => "Unauthorized Seqera Platform API access -- Make sure you have specified the \
                    correct access token"
                .to_owned(),
            _ => error_text(&answer.body)
                .unwrap_or_else(|| format!("Unexpected response for request {}", safe_url(url))),
        };
        Problem {
            url: url.to_owned(),
            code: answer.status,
            message,
        }
    }

    fn of_error(url: &str, error: reqwest::Error) -> Problem {
        let message = match (error.is_connect(), reqwest::Url::parse(url)) {
            (true, Ok(parsed)) => {
                let port = (parsed.port()).map_or(String::new(), |port| format!(":{port}"));
                let host = parsed.host_str().unwrap_or_default();
                format!(
                    "Unable to connect to Seqera Platform API: {}://{host}{port}",
                    parsed.scheme()
                )
            }
            _ => error.without_url().to_string(),
        };
        Problem {
            url: url.to_owned(),
            code: 0,
            message,
        }
    }
}

/// The text of an error answer: its JSON `message`, else its HTML title, else the body.
fn error_text(body: &str) -> Option<String> {
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(body)
        && let Some(message) = json["message"].as_str()
    {
        return Some(message.to_owned());
    }
    let lower = body.to_lowercase();
    if let (Some(open), Some(close)) = (lower.find("<title>"), lower.find("</title>"))
        && open + 7 <= close
    {
        return Some(body[open + 7..close].trim().to_owned());
    }
    Some(body.trim().to_owned()).filter(|body| !body.is_empty())
}

impl Api {
    /// The address of an API path, in the workspace of the run.
    pub(super) fn url(&self, path: &str) -> String {
        match &self.workspace {
            Some(workspace) => format!("{}{path}?workspaceId={workspace}", self.endpoint),
            None => format!("{}{path}", self.endpoint),
        }
    }

    /// Sends a JSON body and gives the JSON of the answer. Busy and failing servers and
    /// timeouts are tried again; a 401 answer refreshes a JWT token once.
    pub(super) fn send(
        &mut self,
        sender: &Sender,
        method: Method,
        path: &str,
        body: &Json,
    ) -> Result<serde_json::Value, Problem> {
        let url = self.url(path);
        let text = body.compact();
        let mut refreshed = false;
        let mut attempt = 1;
        loop {
            let request = (sender.client().request(method.clone(), &url))
                .timeout(self.timeout)
                .header("User-Agent", format!("Nextflow/{NEXTFLOW_VERSION}"))
                .header("Traceparent", traceparent())
                .header("Content-Type", "application/json; charset=utf-8")
                .header("Authorization", self.authorization())
                .body(text.clone());
            let (problem, again) = match sender.send(request) {
                Ok(answer) if (200..300).contains(&answer.status) => {
                    return Ok(serde_json::from_str(&answer.body).unwrap_or_default());
                }
                Ok(answer) if answer.status == 401 && self.refresh.is_some() && !refreshed => {
                    refreshed = true;
                    if self.refresh_token(sender) {
                        continue;
                    }
                    (Problem::of_answer(&url, &answer), false)
                }
                Ok(answer) => (
                    Problem::of_answer(&url, &answer),
                    RETRIED.contains(&answer.status),
                ),
                Err(error) => {
                    let timeout = error.is_timeout();
                    (Problem::of_error(&url, error), timeout)
                }
            };
            if !again || attempt >= self.retry.attempts {
                return Err(problem);
            }
            tracing::debug!(
                code = problem.code,
                attempt,
                "Seqera Platform request tried again"
            );
            std::thread::sleep(self.retry.delay(attempt));
            attempt += 1;
        }
    }

    /// A JWT token is sent as a bearer token, any other token as the password of `@token`.
    fn authorization(&self) -> String {
        let token = &self.token;
        let first = token.split('.').next().unwrap_or_default();
        let decoded = (URL_SAFE_NO_PAD.decode(first.trim_end_matches('=')))
            .or_else(|_| STANDARD.decode(first));
        let json = decoded.is_ok_and(|bytes| bytes.trim_ascii_start().starts_with(b"{"));
        match token.matches('.').count() == 2 || json && token.contains('.') {
            true => format!("Bearer {token}"),
            false => format!("Basic {}", STANDARD.encode(format!("@token:{token}"))),
        }
    }

    /// Gets new tokens for the refresh token; the answer sets them as cookies.
    fn refresh_token(&mut self, sender: &Sender) -> bool {
        let Some(refresh) = &self.refresh else {
            return false;
        };
        let body = format!(
            "grant_type=refresh_token&refresh_token={}",
            form_encode(refresh)
        );
        let request = (sender
            .client()
            .post(format!("{}/oauth/access_token", self.endpoint)))
        .timeout(self.timeout)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body);
        let Ok(answer) = sender.send(request) else {
            return false;
        };
        let mut access = None;
        for cookie in answer.headers.get_all("set-cookie") {
            let text = cookie.to_str().unwrap_or_default();
            let pair = text.split(';').next().unwrap_or_default();
            match pair.split_once('=') {
                Some(("JWT", value)) => access = Some(value.to_owned()),
                Some(("JWT_REFRESH_TOKEN", value)) => self.refresh = Some(value.to_owned()),
                _ => {}
            }
        }
        match access {
            Some(token) => {
                self.token = token;
                true
            }
            None => false,
        }
    }
}

/// A W3C `traceparent` header with new random ids.
fn traceparent() -> String {
    format!("00-{}-{}-01", random_hex(32), random_hex(16))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api(token: &str) -> Api {
        Api {
            endpoint: "http://127.0.0.1:9".into(),
            workspace: Some("12345".into()),
            token: token.into(),
            refresh: None,
            timeout: Duration::from_secs(1),
            retry: Retry {
                delay: Duration::from_millis(350),
                max_delay: Duration::from_secs(90),
                attempts: 10,
                jitter: 0.25,
                multiplier: 2.0,
            },
        }
    }

    #[test]
    fn chooses_authorization() {
        let basic = api("not-a-real-token").authorization();
        assert_eq!(
            basic,
            format!("Basic {}", STANDARD.encode("@token:not-a-real-token"))
        );
        assert!(
            api("eyJhbGciOiJIUzI1NiJ9.e30.sig")
                .authorization()
                .starts_with("Bearer ")
        );
        assert_eq!(
            api("x").url("/trace/create"),
            "http://127.0.0.1:9/trace/create?workspaceId=12345"
        );
    }

    #[test]
    fn reads_error_answers() {
        assert_eq!(error_text(r#"{"message":"Bad"}"#).as_deref(), Some("Bad"));
        let html = "<html><head><title>503 Service Unavailable</title></head></html>";
        assert_eq!(error_text(html).as_deref(), Some("503 Service Unavailable"));
        assert_eq!(error_text(""), None);
    }

    #[test]
    fn grows_delays() {
        let retry = Retry {
            jitter: 0.0,
            ..api("x").retry
        };
        assert_eq!(retry.delay(1), Duration::from_millis(350));
        assert_eq!(retry.delay(2), Duration::from_millis(700));
        assert_eq!(retry.delay(20), Duration::from_secs(90));
        assert_eq!(form_encode("a b/c"), "a+b%2Fc");
    }
}
