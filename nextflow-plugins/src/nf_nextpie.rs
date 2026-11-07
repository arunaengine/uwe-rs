//! nf-nextpie 0.0.2: uploads the run's trace with the pipeline name and version to Nextpie.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::time::Duration;

use nextflow_config::Config;
use nextflow_evaluator::date;
use nextflow_evaluator::value::Value;

use crate::catalogue::{Kind, Plugin, State};
use crate::observer::{Background, Event, Observer, Task};
use crate::service::{self, Sender};

pub(crate) const PLUGIN: Plugin = Plugin {
    scopes: &["nextpie"],
    observer: Some(activate),
    ..Plugin::new("nf-nextpie", Kind::Observer, State::Implemented)
};

/// How long the upload may take.
const LIMIT: Duration = Duration::from_secs(60);
/// How long the end of the run waits for the upload.
const WAIT: Duration = Duration::from_secs(90);
/// The fixed boundary of the plugin's form body.
const BOUNDARY: &str = "----WebKitFormBoundary7MA4YWxkTrZu0gW";
/// Nextflow's default trace columns, the only ones that Nextpie parses.
const COLUMNS: [&str; 14] = [
    "task_id",
    "hash",
    "native_id",
    "name",
    "status",
    "exit",
    "submit",
    "duration",
    "realtime",
    "%cpu",
    "peak_rss",
    "peak_vmem",
    "rchar",
    "wchar",
];
/// A trace value that is not known.
const UNKNOWN: &str = "-";

/// The upload of one run: its address, key, form fields and the trace rows heard so far.
struct Nextpie {
    url: String,
    key: Option<String>,
    fields: [(&'static str, String); 4],
    /// The trace file as configured, whose name the form sends.
    file: String,
    rows: Vec<String>,
    sender: Option<Sender>,
}

/// Turns the plugin on when the run writes a trace and names its pipeline; else it warns and
/// stays off, where Nextflow stops the run without a name.
fn activate(config: &Config, _: &std::path::Path) -> Result<Option<Box<dyn Observer>>, String> {
    let get = |path: &[&str]| {
        (config.get(path))
            .filter(|value| !matches!(value, Value::Null))
            .map(|value| value.to_string())
    };
    if !config
        .get(&["trace", "enabled"])
        .is_some_and(|value| value.truth())
    {
        tracing::warn!("nf-nextpie: the upload is skipped because trace.enabled is not true");
        return Ok(None);
    }
    let mut name = get(&["params", "workflow_name"]);
    let mut version = get(&["params", "workflow_ver"]);
    let manifest = (get(&["manifest", "name"]), get(&["manifest", "version"]));
    if let (Some(manifest_name), Some(manifest_version)) = manifest
        && !manifest_name.is_empty()
        && !manifest_version.is_empty()
    {
        tracing::info!(
            "nf-nextpie: pipeline name {manifest_name} and version label {manifest_version} \
             found in manifest scope"
        );
        (name, version) = (Some(manifest_name), Some(manifest_version));
    }
    let (Some(name), Some(version)) = (name, version) else {
        tracing::warn!(
            "nf-nextpie: cannot extract workflow name and version from the pipeline; set \
             manifest.name and manifest.version, or params.workflow_name and \
             params.workflow_ver; the upload is skipped"
        );
        return Ok(None);
    };
    let host = get(&["nextpie", "host"]).unwrap_or_else(|| "localhost".to_owned());
    let port = get(&["nextpie", "port"]).unwrap_or_else(|| "5000".to_owned());
    let fields = [
        ("Workflow", name),
        ("Version", version),
        (
            "Group",
            get(&["params", "group"]).unwrap_or_else(|| "null".to_owned()),
        ),
        (
            "Project",
            get(&["params", "name"]).unwrap_or_else(|| "null".to_owned()),
        ),
    ];
    let mut nextpie = Nextpie {
        url: format!("http://{host}:{port}/api/v1.0/upload-data"),
        key: get(&["nextpie", "key"]),
        fields,
        file: get(&["trace", "file"]).unwrap_or_else(|| "trace.txt".to_owned()),
        rows: Vec::new(),
        sender: None,
    };
    let background = Background::start("nf-nextpie", WAIT, move |event| nextpie.handle(&event))
        .map_err(|error| error.to_string())?;
    Ok(Some(Box::new(background)))
}

impl Nextpie {
    fn handle(&mut self, event: &Event) {
        match event {
            Event::TaskCompleted(task) | Event::TaskFailed(task) | Event::TaskCached(task) => {
                self.rows.push(row(task));
            }
            Event::RunEnded(_) => self.upload(),
            _ => {}
        }
    }

    /// Posts the form once; the answer is only logged and never changes the run's result.
    fn upload(&mut self) {
        tracing::info!(
            "nf-nextpie: uploading usage data, trace file {}, URI {}",
            self.file,
            service::safe_url(&self.url)
        );
        let body = self.body();
        let sender = match service::sender(&mut self.sender) {
            Ok(sender) => sender,
            Err(error) => return tracing::warn!("nf-nextpie: {error}"),
        };
        let mut request = (sender.client().post(&self.url))
            .timeout(LIMIT)
            .header(
                "Content-Type",
                format!("multipart/form-data; boundary={BOUNDARY}"),
            )
            .body(body);
        if let Some(key) = &self.key {
            request = request.header("X-API-KEY", key);
        }
        match sender.send(request) {
            Ok(answer) if answer.status == 200 => {
                tracing::info!("nf-nextpie: response:\n{}", answer.body);
            }
            Ok(answer) => tracing::warn!(
                "nf-nextpie: unexpected status: {} message: {}",
                answer.status,
                answer.body
            ),
            Err(error) if error.is_connect() || error.is_timeout() => {
                tracing::warn!("nf-nextpie: network issue or Nextpie server is not live");
            }
            Err(error) => tracing::warn!("nf-nextpie: {error}"),
        }
    }

    /// The form: the four text fields, then the trace as a text file.
    fn body(&self) -> String {
        let mut body = String::new();
        for (name, value) in &self.fields {
            body.push_str(&format!(
                "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n\
                 {value}\r\n"
            ));
        }
        let file = self.file.rsplit('/').next().unwrap_or_default();
        body.push_str(&format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"File\"; \
             filename=\"{file}\"\r\nContent-Type: text/plain\r\n\r\n{}\n",
            COLUMNS.join("\t")
        ));
        for row in &self.rows {
            body.push_str(row);
            body.push('\n');
        }
        body.push_str(&format!("\r\n--{BOUNDARY}--\r\n"));
        body
    }
}

/// The trace line of an attempt, as Nextflow writes its default columns.
fn row(task: &Task) -> String {
    let values: Vec<String> = (COLUMNS.iter())
        .map(|column| {
            let value = task.record.get(*column).unwrap_or(&serde_json::Value::Null);
            let number = value.as_f64();
            match (*column, value) {
                (_, serde_json::Value::Null) => UNKNOWN.to_owned(),
                ("submit", _) => number.map_or(UNKNOWN.to_owned(), |millis| {
                    date::format(millis as i64, "yyyy-MM-dd HH:mm:ss.SSS")
                        .unwrap_or_else(|_| UNKNOWN.to_owned())
                }),
                ("duration" | "realtime", _) => duration(number.map(|millis| millis as i64)),
                ("%cpu", _) => number.map_or(UNKNOWN.to_owned(), |share| format!("{share:.1}%")),
                ("peak_rss" | "peak_vmem" | "rchar" | "wchar", _) => {
                    number.map_or(UNKNOWN.to_owned(), |bytes| size(bytes as u64))
                }
                (_, serde_json::Value::String(text)) => text.clone(),
                _ => value.to_string(),
            }
        })
        .collect();
    values.join("\t")
}

/// A duration as Nextflow writes it: `850ms`, `3.4s`, `1m 5s`, `2h 1m 3s`.
fn duration(millis: Option<i64>) -> String {
    let Some(millis) = millis.filter(|millis| *millis >= 0) else {
        return UNKNOWN.to_owned();
    };
    if millis < 1000 {
        return format!("{millis}ms");
    }
    if millis < 60_000 {
        let tenths = (millis as f64 / 100.0).round() as i64;
        return match tenths % 10 {
            0 => format!("{}s", tenths / 10),
            rest => format!("{}.{rest}s", tenths / 10),
        };
    }
    let seconds = (millis as f64 / 1000.0).round() as i64;
    let parts = [
        (seconds / 86_400, "d"),
        (seconds / 3600 % 24, "h"),
        (seconds / 60 % 60, "m"),
        (seconds % 60, "s"),
    ];
    let words: Vec<String> = (parts.iter())
        .filter(|(value, _)| *value > 0)
        .map(|(value, unit)| format!("{value}{unit}"))
        .collect();
    words.join(" ")
}

/// A size as Nextflow writes it: `0`, `512 B`, `91.7 KB`.
fn size(bytes: u64) -> String {
    const UNITS: [&str; 7] = ["B", "KB", "MB", "GB", "TB", "PB", "EB"];
    if bytes == 0 {
        return "0".to_owned();
    }
    let group = (((bytes as f64).log2() / 10.0).floor() as usize).min(6);
    let value = bytes as f64 / 1024f64.powi(group as i32);
    let text = format!("{value:.1}");
    let text = text.strip_suffix(".0").unwrap_or(&text);
    format!("{text} {}", UNITS[group])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_trace_formats() {
        assert_eq!(duration(Some(117)), "117ms");
        assert_eq!(duration(Some(3_420)), "3.4s");
        assert_eq!(duration(Some(65_000)), "1m 5s");
        assert_eq!(duration(None), "-");
        assert_eq!(size(0), "0");
        assert_eq!(size(244), "244 B");
        assert_eq!(size(93_900), "91.7 KB");
        let record = serde_json::json!({
            "task_id": 2, "hash": "ce/3e8a87", "name": "SAY (b)", "status": "COMPLETED",
            "exit": 0, "duration": 117, "realtime": 7, "%cpu": 141.2, "peak_rss": 0,
        });
        let task = Task {
            record: record.as_object().cloned().unwrap_or_default(),
            process_id: None,
            hash: String::new(),
            executor: "local".to_owned(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            values: Vec::new(),
            declared: Vec::new(),
            stdout: String::new(),
            stderr: String::new(),
        };
        assert_eq!(
            row(&task),
            "2\tce/3e8a87\t-\tSAY (b)\tCOMPLETED\t0\t-\t117ms\t7ms\t141.2%\t0\t-\t-\t-"
        );
    }
}
