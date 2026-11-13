//! nf-prometheus 0.1.0: run and task metrics in the Prometheus text format, written to a file,
//! pushed to a Pushgateway and served on `/metrics` while the run lasts.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nextflow_config::Config;
use nextflow_evaluator::value::Value;

use crate::catalogue::{Kind, Plugin, State};
use crate::observer::{Background, End, Event, Observer, Start, Task};
use crate::run_metadata::{trace_number, trace_text};
use crate::service::{self, Sender, form_encode};

pub(crate) const PLUGIN: Plugin = Plugin {
    scopes: &["prometheus"],
    observer: Some(activate),
    ..Plugin::new("nf-prometheus", Kind::Observer, State::Implemented)
};

const CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";
/// The least time between two pushes that no run event forces.
const PUSH_GAP: Duration = Duration::from_secs(5);
const PUSH_LIMIT: Duration = Duration::from_secs(10);
/// How long the end of the run waits for the last push.
const WAIT: Duration = Duration::from_secs(30);

/// The metric families: name, type and help text.
const FAMILIES: [(&str, &str, &str); 8] = [
    (
        "nf_task_cpus_requested_total",
        "counter",
        "Sum of CPUs requested by completed tasks, by process",
    ),
    (
        "nf_task_peak_rss_bytes_max",
        "gauge",
        "Maximum peak RSS observed among tasks, by process",
    ),
    (
        "nf_task_queue_seconds_total",
        "counter",
        "Total time tasks spent queued (submit to start), by process",
    ),
    (
        "nf_task_realtime_seconds_total",
        "counter",
        "Total task execution wall time, by process",
    ),
    (
        "nf_tasks_total",
        "counter",
        "Tasks by process and final status",
    ),
    (
        "nf_workflow_duration_seconds",
        "gauge",
        "Wall clock duration of the workflow run",
    ),
    (
        "nf_workflow_info",
        "gauge",
        "Workflow run information (always 1)",
    ),
    (
        "nf_workflow_status",
        "gauge",
        "Workflow status: 0=running 1=complete 2=error",
    ),
];

fn activate(
    config: &Config,
    launch: &std::path::Path,
    _: &crate::catalogue::Environment,
) -> Result<Option<Box<dyn Observer>>, String> {
    let get = |key: &str| {
        (config.get(&["prometheus", key])).filter(|value| !matches!(value, Value::Null))
    };
    if !get("enabled").is_none_or(|value| value.truth()) {
        return Ok(None);
    }
    let text = |key: &str| get(key).map(|value| value.to_string());
    let launch = launch.to_path_buf();
    let path = launch.join(text("path").unwrap_or_else(|| "nf-prometheus.prom".into()));
    let push = (text("pushgateway")).map(|url| url.trim_end_matches('/').to_owned());
    let port = (text("httpPort")).and_then(|port| port.parse::<u16>().ok());
    // The plugin serves all interfaces; uwe binds the loopback unless the config widens it.
    let address = text("httpAddress").unwrap_or_else(|| "127.0.0.1".into());
    let mut exporter = Exporter {
        path,
        push,
        job: text("pushJob").unwrap_or_else(|| "nextflow".into()),
        serve: port.map(|port| (address, port)),
        server: None,
        sender: None,
        run_name: String::from("unknown"),
        started: 0,
        samples: BTreeMap::new(),
        errored: false,
        pushed: None,
        warned: false,
    };
    let background = Background::start("nf-prometheus", WAIT, move |event| {
        exporter.handle(&event);
    })
    .map_err(|error| error.to_string())?;
    Ok(Some(Box::new(background)))
}

/// Keeps the samples and sends them out in the worker thread.
struct Exporter {
    path: PathBuf,
    push: Option<String>,
    job: String,
    /// The address and port of the `/metrics` server, when it is wanted.
    serve: Option<(String, u16)>,
    server: Option<Server>,
    sender: Option<Sender>,
    run_name: String,
    started: i64,
    /// Values by metric name, then by the text of their labels.
    samples: BTreeMap<&'static str, BTreeMap<String, f64>>,
    errored: bool,
    pushed: Option<Instant>,
    /// Only the first failed push is shown.
    warned: bool,
}

impl Exporter {
    fn handle(&mut self, event: &Event) {
        let forced = match event {
            Event::RunStarted(start) => {
                self.started(start);
                false
            }
            Event::TaskSubmitted(task) => {
                self.add(
                    "nf_tasks_total",
                    &self.task_labels(task, Some("submitted")),
                    1.0,
                );
                false
            }
            Event::TaskCompleted(task) | Event::TaskFailed(task) => {
                self.ended_task(task);
                false
            }
            Event::TaskCached(task) => {
                self.add(
                    "nf_tasks_total",
                    &self.task_labels(task, Some("cached")),
                    1.0,
                );
                false
            }
            Event::RunEnded(end) => {
                self.ended(end);
                true
            }
            Event::TaskStarted(_) | Event::FilePublished { .. } => return,
        };
        let text = self.render();
        self.write(&text);
        if let Some(server) = &self.server
            && let Ok(mut shown) = server.text.lock()
        {
            shown.clone_from(&text);
        }
        if forced
            || self
                .pushed
                .is_none_or(|pushed| pushed.elapsed() >= PUSH_GAP)
        {
            self.send(text);
        }
        if let (Event::RunEnded(_), Some(server)) = (event, self.server.take()) {
            server.stop();
        }
    }

    fn started(&mut self, start: &Start) {
        if !start.run_name.is_empty() {
            self.run_name = start.run_name.clone();
        }
        self.started = start.start;
        if let Some((address, port)) = &self.serve {
            match Server::start(address, *port) {
                Ok(server) => {
                    tracing::info!(
                        "nf-prometheus: serving metrics on http://{}/metrics",
                        server.address
                    );
                    self.server = Some(server);
                }
                Err(error) => tracing::warn!(
                    "nf-prometheus: cannot start metrics HTTP server on port {port}: {error}"
                ),
            }
        }
        let run = label_text(&[("run_name", &self.run_name)]);
        let info = label_text(&[
            ("run_name", &self.run_name),
            ("session_id", &start.session_id),
        ]);
        self.set("nf_workflow_info", info, 1.0);
        self.set("nf_workflow_status", run, 0.0);
    }

    fn ended_task(&mut self, task: &Task) {
        let status = trace_text(task, "status").to_lowercase();
        self.add(
            "nf_tasks_total",
            &self.task_labels(task, Some(&status)),
            1.0,
        );
        let labels = self.task_labels(task, None);
        let (submit, start) = (trace_number(task, "submit"), trace_number(task, "start"));
        if let (Some(submit), Some(start)) = (submit, start)
            && start >= submit
        {
            self.add(
                "nf_task_queue_seconds_total",
                &labels,
                (start - submit) / 1000.0,
            );
        }
        if let Some(realtime) = trace_number(task, "realtime") {
            self.add("nf_task_realtime_seconds_total", &labels, realtime / 1000.0);
        }
        // A process without `cpus` asks for Nextflow's default of one, which uwe leaves null.
        let cpus = trace_number(task, "cpus").unwrap_or(1.0);
        self.add("nf_task_cpus_requested_total", &labels, cpus);
        if let Some(peak) = trace_number(task, "peak_rss") {
            let samples = self
                .samples
                .entry("nf_task_peak_rss_bytes_max")
                .or_default();
            let value = samples.entry(labels).or_insert(peak);
            *value = value.max(peak);
        }
    }

    /// The run ended: a failed run keeps the error status, as after Nextflow's error event.
    fn ended(&mut self, end: &End) {
        self.errored |= !end.success;
        let run = label_text(&[("run_name", &self.run_name)]);
        let status = if self.errored { 2.0 } else { 1.0 };
        self.set("nf_workflow_status", run.clone(), status);
        let seconds = (end.end - self.started) as f64 / 1000.0;
        self.set("nf_workflow_duration_seconds", run, seconds);
    }

    fn task_labels(&self, task: &Task, status: Option<&str>) -> String {
        let process = Some(trace_text(task, "process")).filter(|process| !process.is_empty());
        let mut labels = vec![
            ("run_name", self.run_name.as_str()),
            ("process", process.unwrap_or("unknown")),
        ];
        labels.extend(status.map(|status| ("status", status)));
        label_text(&labels)
    }

    fn add(&mut self, name: &'static str, labels: &str, amount: f64) {
        let samples = self.samples.entry(name).or_default();
        *samples.entry(labels.to_owned()).or_insert(0.0) += amount;
    }

    fn set(&mut self, name: &'static str, labels: String, value: f64) {
        self.samples.entry(name).or_default().insert(labels, value);
    }

    /// The text exposition format 0.0.4: families by name, each with its help, type and
    /// samples ordered by their labels; families without samples are left out.
    fn render(&self) -> String {
        let mut text = String::new();
        for (name, kind, help) in FAMILIES {
            let Some(samples) = self.samples.get(name).filter(|samples| !samples.is_empty()) else {
                continue;
            };
            text.push_str(&format!("# HELP {name} {help}\n# TYPE {name} {kind}\n"));
            for (labels, value) in samples {
                text.push_str(&format!("{name}{labels} {}\n", number(*value)));
            }
        }
        text
    }

    /// Replaces the file at once: the text goes to `<path>.tmp`, which then takes its name.
    fn write(&self, text: &str) {
        let mut temporary = self.path.clone().into_os_string();
        temporary.push(".tmp");
        let temporary = PathBuf::from(temporary);
        let written = (self.path.parent())
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&temporary, text))
            .and_then(|()| std::fs::rename(&temporary, &self.path));
        if let Err(error) = written {
            tracing::warn!("nf-prometheus: cannot write metrics file: {error}");
        }
    }

    /// Puts the text to the Pushgateway under the job and run name, once; only the first
    /// failure is shown.
    fn send(&mut self, text: String) {
        let Some(base) = &self.push else {
            return;
        };
        self.pushed = Some(Instant::now());
        let part = |text: &str| match text.is_empty() {
            true => "unknown".to_owned(),
            false => form_encode(text),
        };
        let url = format!(
            "{base}/metrics/job/{}/run_name/{}",
            part(&self.job),
            part(&self.run_name)
        );
        let sender = match service::sender(&mut self.sender) {
            Ok(sender) => sender,
            Err(error) => return tracing::warn!("nf-prometheus: {error}"),
        };
        let request = (sender.client().put(&url))
            .timeout(PUSH_LIMIT)
            .header("Content-Type", CONTENT_TYPE)
            .body(text);
        let problem = match sender.send(request) {
            Ok(answer) if (200..300).contains(&answer.status) => return,
            Ok(answer) => Push::Refused(answer.status),
            Err(error) => Push::Failed(error.without_url().to_string()),
        };
        let problem = push_warning(&url, &problem);
        if !self.warned {
            self.warned = true;
            tracing::warn!("nf-prometheus: {problem}");
        }
    }
}

/// How a push failed.
enum Push {
    Refused(u16),
    Failed(String),
}

/// The warning of a failed push, with the address shown without its credentials.
fn push_warning(url: &str, push: &Push) -> String {
    let url = service::safe_url(url);
    match push {
        Push::Refused(status) => format!("pushgateway returned HTTP {status} for {url}"),
        Push::Failed(message) => format!("cannot reach pushgateway at {url}: {message}"),
    }
}

/// Labels as the format writes them, like `{run_name="a",process="B"}`.
fn label_text(labels: &[(&str, &str)]) -> String {
    let labels: Vec<String> = (labels.iter())
        .map(|(key, value)| {
            let value = (value.replace('\\', "\\\\"))
                .replace('"', "\\\"")
                .replace('\n', "\\n");
            format!("{key}=\"{value}\"")
        })
        .collect();
    format!("{{{}}}", labels.join(","))
}

/// A value without a fraction when it is whole, else the shortest decimal that reads back
/// to the same number.
fn number(value: f64) -> String {
    format!("{value}")
}

/// The `/metrics` server of a run: a thread that answers one connection at a time.
struct Server {
    address: SocketAddr,
    text: Arc<Mutex<String>>,
    stopped: Arc<AtomicBool>,
}

impl Server {
    fn start(address: &str, port: u16) -> std::io::Result<Server> {
        let listener = TcpListener::bind((address, port))?;
        let address = listener.local_addr()?;
        let text = Arc::new(Mutex::new(String::new()));
        let stopped = Arc::new(AtomicBool::new(false));
        let (shown, stop) = (text.clone(), stopped.clone());
        std::thread::Builder::new()
            .name("nf-prometheus server".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    if let Ok(stream) = stream {
                        answer(stream, &shown);
                    }
                }
            })?;
        Ok(Server {
            address,
            text,
            stopped,
        })
    }

    /// Ends the server: its thread sees the flag at the next connection, which this makes.
    fn stop(self) {
        self.stopped.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect_timeout(&self.address, Duration::from_secs(1));
    }
}

/// Answers one request: the metrics for `GET /metrics`, 405 for other methods there, and
/// 404 elsewhere.
fn answer(stream: TcpStream, text: &Mutex<String>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut reader = BufReader::new(&stream);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let mut parts = line.split_whitespace();
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let metrics =
        path == "/metrics" || path.starts_with("/metrics/") || path.starts_with("/metrics?");
    let response = match (metrics, method) {
        (true, "GET") => {
            let body = text.lock().map(|text| text.clone()).unwrap_or_default();
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {CONTENT_TYPE}\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{body}",
                body.len()
            )
        }
        (true, _) => {
            "HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .into()
        }
        (false, _) => {
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into()
        }
    };
    let _ = (&stream).write_all(response.as_bytes());
}

#[cfg(test)]
mod tests {
    use std::io::Read as _;

    use super::*;

    fn request(address: SocketAddr, line: &str) -> String {
        let mut stream = TcpStream::connect(address).expect("connects");
        stream
            .write_all(format!("{line}\r\nHost: x\r\n\r\n").as_bytes())
            .expect("sends");
        let mut answer = String::new();
        stream
            .read_to_string(&mut answer)
            .expect("reads the answer");
        answer
    }

    #[test]
    fn serves_metrics() {
        let server = Server::start("127.0.0.1", 0).expect("starts");
        assert!(server.address.ip().is_loopback());
        *server.text.lock().expect("not poisoned") = "nf_workflow_status 0\n".into();
        let answer = request(server.address, "GET /metrics HTTP/1.1");
        assert!(answer.starts_with("HTTP/1.1 200 OK\r\n"), "{answer}");
        assert!(
            answer.contains(&format!("Content-Type: {CONTENT_TYPE}\r\n")),
            "{answer}"
        );
        assert!(
            answer.ends_with("\r\n\r\nnf_workflow_status 0\n"),
            "{answer}"
        );
        let refused = request(server.address, "POST /metrics HTTP/1.1");
        assert!(refused.starts_with("HTTP/1.1 405 "), "{refused}");
        let missing = request(server.address, "GET / HTTP/1.1");
        assert!(missing.starts_with("HTTP/1.1 404 "), "{missing}");
        server.stop();
    }

    #[test]
    fn writes_text_format() {
        assert_eq!(label_text(&[("a", "x\"y\\z\nw")]), r#"{a="x\"y\\z\nw"}"#);
        assert_eq!(number(1.0), "1");
        assert_eq!(number(6_991_872.0), "6991872");
        assert_eq!(number(0.185), "0.185");
        assert_eq!(number(5.35), "5.35");
    }

    #[test]
    fn hides_push_credentials() {
        let url = "https://user:secret@push.example.org/metrics/job/nf/run_name/a?token=abc";
        let refused = push_warning(url, &Push::Refused(401));
        assert_eq!(
            refused,
            "pushgateway returned HTTP 401 for https://push.example.org/metrics/job/nf/run_name/a"
        );
        let failed = push_warning(url, &Push::Failed("connection refused".into()));
        assert!(
            !failed.contains("secret") && !failed.contains("token"),
            "{failed}"
        );
    }
}
