//! nf-crg-genomics-lims 0.1.1: tells a LIMS whether the run succeeded with one `PATCH` at its
//! end, then uploads one file; off unless `lims.enabled` or the param `lims_enabled` is true.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;
use std::time::Duration;

use nextflow_config::Config;
use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::value::Value;
use serde_json::{Map, Value as Json};

use crate::Context;
use crate::catalogue::{Kind, Plugin, State};
use crate::observer::{Background, End, Event, Observer};
use crate::report_format::now;
use crate::service::{self, Sender};

pub(crate) const PLUGIN: Plugin = Plugin {
    functions: &["sayHello"],
    call: Some(call),
    scopes: &["lims"],
    observer: Some(activate),
    ..Plugin::new("nf-crg-genomics-lims", Kind::Observer, State::Implemented)
};

/// The status update may take 15 s to connect and 30 s to answer; the upload 60 s.
const STATUS_LIMIT: Duration = Duration::from_secs(45);
const UPLOAD_LIMIT: Duration = Duration::from_secs(60);
/// How long the end of the run waits for both requests.
const WAIT: Duration = Duration::from_secs(120);

fn activate(
    config: &Config,
    _: &std::path::Path,
    _: &crate::catalogue::Environment,
) -> Result<Option<Box<dyn Observer>>, String> {
    let enabled = match config.get(&["params", "lims_enabled"]) {
        Some(Value::Null) | None => {
            (config.get(&["lims", "enabled"])).is_some_and(|value| value.truth())
        }
        Some(flag) => flag.truth() && flag.to_string() != "false",
    };
    if !enabled {
        return Ok(None);
    }
    let mut lims = Lims {
        sender: None,
        start: None,
        done: false,
    };
    let background = Background::start("nf-crg-genomics-lims", WAIT, move |event| {
        lims.handle(&event);
    })
    .map_err(|error| error.to_string())?;
    Ok(Some(Box::new(background)))
}

/// `sayHello(target)`, a demo function the plugin still ships.
fn call(name: &str, arguments: Vec<Value>, context: &mut Context) -> Result<Value, Error> {
    match (name, arguments.first()) {
        ("sayHello", Some(target)) => {
            context.host.print(&format!("Hello, {target}!\n"));
            Ok(Value::Null)
        }
        _ => Err(Error::Unsupported {
            message: format!("the function `{name}` of the plugin `nf-crg-genomics-lims`"),
            span: Default::default(),
        }),
    }
}

struct Lims {
    sender: Option<Sender>,
    /// The params and launch directory of the run.
    start: Option<(Json, String)>,
    done: bool,
}

impl Lims {
    fn handle(&mut self, event: &Event) {
        match event {
            Event::RunStarted(start) => {
                self.start = Some((start.params.clone(), start.launch_dir.clone()));
            }
            // Nextflow 26.04.6 runs the end twice for a failed run; uwe sends once.
            Event::RunEnded(end) if !std::mem::replace(&mut self.done, true) => self.ended(end),
            _ => {}
        }
    }

    /// Sends the status of the run, then uploads the file, whatever the status answer was.
    fn ended(&mut self, end: &End) {
        let Some((params, launch)) = self.start.clone() else {
            return;
        };
        let param = |name: &str| match &params[name] {
            Json::Null => None,
            Json::String(text) if text.is_empty() => None,
            Json::String(text) => Some(text.clone()),
            other => Some(other.to_string()),
        };
        let (Some(base), Some(id)) = (
            param("lims_api_base_url"),
            param("lims_pipeline_execution_id"),
        ) else {
            return tracing::info!(
                "LIMS status update skipped: base URL or execution ID not configured."
            );
        };
        let auth = match (param("lims_username"), param("lims_api_key")) {
            (Some(user), Some(key)) => Some(format!("ApiKey {user}:{key}")),
            _ => None,
        };
        let sender = match service::sender(&mut self.sender) {
            Ok(sender) => sender,
            Err(error) => return tracing::warn!("Error updating LIMS status: {error}"),
        };
        let base = base.trim_end_matches('/');
        let status = match end.success {
            true => param("lims_status_success").unwrap_or_else(|| "SU".to_owned()),
            false => param("lims_status_failure").unwrap_or_else(|| "FA".to_owned()),
        };
        tracing::info!("Updating LIMS status to {status} for execution ID: {id} ...");
        let key = param("lims_status_key").unwrap_or_else(|| "status".to_owned());
        let body = Json::Object(Map::from_iter([(key, Json::String(status))]));
        let mut request = (sender.client().patch(format!("{base}/{id}")))
            .timeout(STATUS_LIMIT)
            .header("Content-Type", "application/json")
            .body(body.to_string());
        if let Some(auth) = &auth {
            request = request.header("Authorization", auth);
        }
        match sender.send(request) {
            Ok(answer) if (200..300).contains(&answer.status) => {
                tracing::info!(
                    "LIMS response: SUCCESS ({}) - {}",
                    answer.status,
                    answer.body
                );
            }
            Ok(answer) => {
                tracing::warn!(
                    "LIMS response: FAILED ({}) - {}",
                    answer.status,
                    answer.body
                );
            }
            Err(error) => tracing::warn!("Error updating LIMS status: {}", error.without_url()),
        }
        let Some(file) = param("lims_upload_file") else {
            return;
        };
        let file = Path::new(&launch).join(file);
        if !file.is_file() {
            return tracing::warn!(
                "LIMS upload skipped: File does not exist at {}",
                file.display()
            );
        }
        let upload = Upload {
            url: param("lims_upload_url").unwrap_or_else(|| format!("{base}/{id}/upload")),
            method: param("lims_upload_method")
                .unwrap_or_else(|| "POST".to_owned())
                .to_uppercase(),
            binary: param("lims_upload_mode")
                .is_some_and(|mode| mode.eq_ignore_ascii_case("binary")),
            field: param("lims_upload_form_field").unwrap_or_else(|| "file".to_owned()),
        };
        if let Err(error) = upload.send(sender, &file, auth.as_deref()) {
            tracing::warn!("Error uploading file to LIMS: {error}");
        }
    }
}

/// How the file goes to the LIMS.
struct Upload {
    url: String,
    method: String,
    binary: bool,
    field: String,
}

impl Upload {
    fn send(&self, sender: &Sender, file: &Path, auth: Option<&str>) -> Result<(), String> {
        let mode = if self.binary { "binary" } else { "multipart" };
        tracing::info!(
            "Uploading file {} to {} using {} ({mode}) ...",
            file.display(),
            service::safe_url(&self.url),
            self.method
        );
        let bytes = std::fs::read(file).map_err(|error| error.to_string())?;
        let name = file
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
        let name = name.unwrap_or_default();
        let kind = content_type(&name);
        let (kind, body) = match self.binary {
            true => (kind.to_owned(), bytes),
            false => {
                let boundary = format!("NextflowLimsBoundary{}", now());
                let mut body = format!(
                    "--{boundary}\r\nContent-Disposition: form-data; name=\"{}\"; \
                     filename=\"{name}\"\r\nContent-Type: {kind}\r\n\r\n",
                    self.field
                )
                .into_bytes();
                body.extend(bytes);
                body.extend(format!("\r\n--{boundary}--\r\n").into_bytes());
                (format!("multipart/form-data; boundary={boundary}"), body)
            }
        };
        let method = reqwest::Method::from_bytes(self.method.as_bytes())
            .map_err(|error| error.to_string())?;
        let mut request = (sender.client().request(method, &self.url))
            .timeout(UPLOAD_LIMIT)
            .header("Content-Type", kind)
            .body(body);
        if let Some(auth) = auth {
            request = request.header("Authorization", auth);
        }
        let answer = (sender.send(request)).map_err(|error| error.without_url().to_string())?;
        match (200..300).contains(&answer.status) {
            true => tracing::info!(
                "LIMS file upload response: SUCCESS ({}) - {}",
                answer.status,
                answer.body
            ),
            false => tracing::warn!(
                "LIMS file upload response: FAILED ({}) - {}",
                answer.status,
                answer.body
            ),
        }
        Ok(())
    }
}

/// The media type of a file by its name, as Java guesses it from the usual extensions.
fn content_type(name: &str) -> &'static str {
    let extension = name
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_lowercase());
    match extension.as_deref() {
        Some("html" | "htm") => "text/html",
        Some("txt" | "log") => "text/plain",
        Some("csv") => "text/csv",
        Some("tsv") => "text/tab-separated-values",
        Some("json") => "application/json",
        Some("xml") => "application/xml",
        Some("pdf") => "application/pdf",
        Some("zip") => "application/zip",
        Some("gz") => "application/gzip",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("svg") => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hides_failed_address() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("binds a free port");
        let port = listener.local_addr().expect("an address").port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                drop(stream);
            }
        });
        let directory = tempfile::tempdir().expect("a temporary directory");
        let file = directory.path().join("report.txt");
        std::fs::write(&file, "report").expect("writes a file");
        let upload = Upload {
            url: format!("http://user:pw-secret@127.0.0.1:{port}/up?token=query-secret"),
            method: "POST".into(),
            binary: true,
            field: "file".into(),
        };
        let sender = Sender::new().expect("a sender");
        let error = upload
            .send(&sender, &file, None)
            .expect_err("the server hangs up");
        assert!(!error.is_empty() && !error.contains("secret"), "{error}");
    }
}
