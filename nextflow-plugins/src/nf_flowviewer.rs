//! nf-flowviewer 1.0.0: sends the session, process graph, task states and published files of a run
//! to a FlowViewer server.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use nextflow_config::Config;
use nextflow_evaluator::value::Value;

use crate::catalogue::{Kind, Plugin, State};
use crate::observer::{Background, Event, Observer, Start, Task};
use crate::report_format::Json;

pub(crate) const PLUGIN: Plugin = Plugin {
    scopes: &["flowViewer"],
    observer: Some(activate),
    ..Plugin::new("nf-flowviewer", Kind::Observer, State::Implemented)
};

/// How long one request may take, and how long the end of a run waits for those left.
const LIMIT: Duration = Duration::from_secs(30);
const WAIT: Duration = Duration::from_secs(60);

/// The telemetry keys of a finished task, with the trace fields they come from.
const TELEMETRY: [(&str, &str); 26] = [
    ("realtime", "realtime"),
    ("startTime", "start"),
    ("completeTime", "complete"),
    ("cpuPercent", "%cpu"),
    ("cpus", "cpus"),
    ("cpu_model", "cpu_model"),
    ("mem", "%mem"),
    ("rss", "rss"),
    ("vmem", "vmem"),
    ("peakRss", "peak_rss"),
    ("peakVmem", "peak_vmem"),
    ("memory", "memory"),
    ("rchar", "rchar"),
    ("wchar", "wchar"),
    ("readBytes", "read_bytes"),
    ("writeBytes", "write_bytes"),
    ("container", "container"),
    ("disk", "disk"),
    ("module", "module"),
    ("exitStatus", "exit"),
    ("hostname", "hostname"),
    ("inv_ctxt", "inv_ctxt"),
    ("vol_ctxt", "vol_ctxt"),
    ("error_action", "error_action"),
    ("queue", "queue"),
    ("scratch", "scratch"),
];

/// The `flowViewer` scope.
struct Settings {
    /// The base address; requests to `null/api/...` fail, as in the plugin.
    server: String,
    workflow: String,
    run_by: Option<String>,
    /// The bytes read from the start of each published file.
    sample: usize,
}

fn activate(config: &Config, _: &std::path::Path) -> Result<Option<Box<dyn Observer>>, String> {
    let text = |key: &str| config.get(&["flowViewer", key]).filter(Value::truth);
    let sample = text("artifactMaxSize")
        .and_then(|value| value.to_string().parse::<usize>().ok())
        .filter(|size| *size > 0)
        .unwrap_or(512);
    let run_by = std::env::var("FLOWVIEWER_RUN_BY")
        .ok()
        .filter(|it| !it.is_empty());
    let settings = Settings {
        server: text("serverUrl").map_or_else(|| "null".into(), |url| url.to_string()),
        workflow: text("workflowName")
            .map(|it| it.to_string())
            .unwrap_or_default(),
        run_by: run_by.or_else(|| text("runBy").map(|it| it.to_string())),
        sample,
    };
    let mut sender = Sender {
        settings,
        session: None,
        runtime: None,
    };
    let worker = Background::start("nf-flowviewer", WAIT, move |event| sender.send(&event));
    let worker = worker.map_err(|error| format!("nf-flowviewer: cannot start: {error}"))?;
    Ok(Some(Box::new(worker)))
}

/// Sends the events of a run, one request after the other, on the worker thread.
struct Sender {
    settings: Settings,
    /// The id that the server gave the session; requests that need it wait for it.
    session: Option<i64>,
    runtime: Option<tokio::runtime::Runtime>,
}

impl Sender {
    fn send(&mut self, event: &Event) {
        match event {
            Event::RunStarted(start) => {
                let answer = self.request("POST", "/api/session", &self.session_data(start));
                self.session = answer
                    .filter(|(status, _)| *status == 201)
                    .and_then(|(_, body)| body["id"].as_i64());
                self.request("POST", "/api/processes", &self.graph(start));
                self.status("RUNNING");
            }
            Event::TaskSubmitted(_) => {}
            Event::TaskStarted(task) => self.process(task, "RUNNING", "start", false),
            Event::TaskCompleted(task) | Event::TaskFailed(task) => {
                let failed = task.record["exit"].as_i64() != Some(0);
                let status = if failed { "FAILED" } else { "SUCCEEDED" };
                self.process(task, status, "complete", true);
            }
            Event::TaskCached(task) => self.process(task, "CACHED", "complete", false),
            Event::FilePublished { source, .. } => {
                if let Some(artifact) = self.artifact(source) {
                    self.request("POST", "/api/artifacts", &artifact);
                }
            }
            Event::RunEnded(end) => self.status(if end.success { "SUCCEEDED" } else { "FAILED" }),
        }
    }

    fn session_id(&self) -> Json {
        self.session.map_or(Json::Null, Json::from)
    }

    fn status(&mut self, status: &str) {
        let Some(session) = self.session else {
            return;
        };
        let body = Json::Object(Vec::new()).with("status", status);
        self.request("PUT", &format!("/api/sessions/{session}/status"), &body);
    }

    fn session_data(&self, start: &Start) -> Json {
        let script = Path::new(&start.script);
        let name = script
            .file_name()
            .map(|it| it.to_string_lossy().into_owned());
        let folder = Path::new(&start.work_dir)
            .parent()
            .unwrap_or(Path::new(&start.launch_dir));
        let notes = name.as_deref().map(|name| name.replace(".nf", ".md"));
        let info = [notes.as_deref().unwrap_or("README.md"), "README.md"]
            .into_iter()
            .find_map(|file| std::fs::read_to_string(folder.join(file)).ok())
            .map(|text| text.lines().collect::<Vec<_>>().join("\n"));
        let pool = std::thread::available_parallelism().map_or(1, |cpus| cpus.get() as u64);
        let text = |value: &Option<String>| value.clone().map_or(Json::Null, Json::from);
        let data = Json::Object(Vec::new())
            .with("uniqueId", start.session_id.as_str())
            .with("workflowName", self.settings.workflow.as_str())
            .with("sessionInfo", text(&info))
            .with("scriptName", text(&name))
            .with("runName", start.run_name.as_str())
            .with("runBy", text(&self.settings.run_by))
            .with("profile", start.profile.as_str())
            .with("ansiLog", false)
            .with("baseDir", start.project_dir.as_str())
            .with("binDir", Json::Null)
            .with("bucketDir", Json::Null)
            .with("workDir", start.work_dir.as_str())
            .with("commandLine", start.command_line.as_str())
            .with("commitId", text(&start.commit_id))
            .with("poolSize", pool)
            .with("resolvedConfig", Json::Null);
        Json::Object(Vec::new()).with("sessionData", data)
    }

    /// The process graph: a vertex per process, numbered from 0 in the order of the calls, and
    /// an edge per channel between processes, with open ends for a run's inputs and outputs.
    fn graph(&self, start: &Start) -> Json {
        let session = self.session_id();
        let vertex = |id: u32| Json::from(i64::from(id) - 1);
        let mut processes = Vec::new();
        let mut ends: Vec<(Json, Json)> = Vec::new();
        for process in &start.processes {
            let fed = start
                .processes
                .iter()
                .any(|other| other.feeds.contains(&process.id));
            if !fed {
                ends.push((Json::Null, vertex(process.id)));
            }
            for target in &process.feeds {
                ends.push((vertex(process.id), vertex(*target)));
            }
            if process.feeds.is_empty() {
                ends.push((vertex(process.id), Json::Null));
            }
            let workflow = process.name.rsplit_once(':').map_or("", |(outer, _)| outer);
            let workflow = workflow.rsplit(':').next().unwrap_or_default();
            processes.push(
                Json::Object(Vec::new())
                    .with("vertexId", vertex(process.id))
                    .with("sessionId", session.clone())
                    .with("processId", u64::from(process.id))
                    .with("label", process.name.rsplit(':').next().unwrap_or_default())
                    .with("type", "PROCESS")
                    .with("workflow", workflow)
                    .with("status", "CREATED"),
            );
        }
        let edges: Vec<Json> = (ends.into_iter().enumerate())
            .map(|(id, (from, to))| {
                Json::Object(Vec::new())
                    .with("id", id as u64)
                    .with("label", Json::Null)
                    .with("fromId", from)
                    .with("toId", to)
            })
            .collect();
        let edges = Json::Object(Vec::new())
            .with("sessionId", session)
            .with("edges", edges);
        Json::Object(Vec::new())
            .with("processes", processes)
            .with("edges", edges)
    }

    fn process(&mut self, task: &Task, status: &str, time: &str, telemetry: bool) {
        let record = &task.record;
        let field = |key: &str| Json::from(record.get(key).unwrap_or(&serde_json::Value::Null));
        let telemetry = match telemetry {
            false => Json::Null,
            true => {
                let mut values = Json::Object(Vec::new()).with("hash", field("hash"));
                for (key, source) in TELEMETRY {
                    values.insert(key, field(source));
                }
                if record.get("module").is_none_or(serde_json::Value::is_null) {
                    values.insert("module", "[]");
                }
                values
            }
        };
        let process_id = task
            .process_id
            .map_or(Json::Null, |id| Json::from(u64::from(id)));
        let body = Json::Object(Vec::new())
            .with("sessionId", self.session_id())
            .with("processId", process_id)
            .with("status", status)
            .with("time", field(time))
            .with("telemetry", telemetry);
        self.request("PUT", "/api/process", &body);
    }

    /// The published file as the server takes it, from its source: the first bytes as text.
    fn artifact(&self, source: &str) -> Option<Json> {
        self.session?;
        let path = Path::new(source.strip_prefix("file://").unwrap_or(source));
        let found = std::fs::metadata(path).ok()?;
        let mut data = Vec::new();
        let file = std::fs::File::open(path).ok()?;
        file.take(self.settings.sample as u64)
            .read_to_end(&mut data)
            .ok()?;
        let scheme = source
            .split_once("://")
            .map_or("file", |(scheme, _)| scheme);
        let uri = match source.contains("://") {
            true => source.to_owned(),
            false => format!("file://{source}"),
        };
        let name = path.file_name().map(|it| it.to_string_lossy().into_owned());
        let artifact = Json::Object(Vec::new())
            .with("sessionId", self.session_id())
            .with("name", name.unwrap_or_default())
            .with("uri", uri)
            .with("size", found.len())
            .with("permissions", permissions(found.permissions().mode()))
            .with("fileSystem", scheme)
            .with("data", String::from_utf8_lossy(&data).into_owned())
            .with("hash", work_hash(source));
        Some(Json::Object(Vec::new()).with("artifact", artifact))
    }

    /// Sends one request; the status and JSON body of the answer, `None` when it failed.
    fn request(
        &mut self,
        method: &str,
        path: &str,
        body: &Json,
    ) -> Option<(u16, serde_json::Value)> {
        let client = storage::http_client().ok()?;
        if self.runtime.is_none() {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            self.runtime = runtime.ok();
        }
        let url = format!("{}{path}", self.settings.server);
        let request = match method {
            "PUT" => client.put(url),
            _ => client.post(url),
        };
        let request = (request.timeout(LIMIT))
            .header("Content-Type", "application/json")
            .body(body.compact());
        let answer = self.runtime.as_ref()?.block_on(async {
            let response = request.send().await.ok()?;
            let status = response.status().as_u16();
            let text = response.text().await.unwrap_or_default();
            let body = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
            Some((status, body))
        });
        if answer.is_none() {
            tracing::debug!(path, "a FlowViewer request failed");
        }
        answer
    }
}

/// The POSIX permissions of a mode, like `rw-r--r--`.
fn permissions(mode: u32) -> String {
    let letters = ['r', 'w', 'x'];
    (0..9)
        .map(|bit| match mode & (0o400 >> bit) != 0 {
            true => letters[bit % 3],
            false => '-',
        })
        .collect()
}

/// The task hash in a Nextflow work path, like `ca/1237...` of `work/ca/1237.../x`; empty when
/// the path is not in a work directory, as uwe's stored files are not.
fn work_hash(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').collect();
    let hex = |text: &str| text.chars().all(|it| it.is_ascii_hexdigit());
    for window in parts.windows(3) {
        if window[0] == "work"
            && window[1].len() == 2
            && hex(window[1])
            && window[2].len() >= 30
            && hex(window[2])
        {
            return format!("{}/{}", window[1], window[2]);
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_files() {
        assert_eq!(permissions(0o100644), "rw-r--r--");
        assert_eq!(permissions(0o755), "rwxr-xr-x");
        let path = "/LAUNCH/work/ca/123723413158457fe79afc7cae43e4/part_02.txt";
        assert_eq!(work_hash(path), "ca/123723413158457fe79afc7cae43e4");
        assert_eq!(work_hash("/store/cas/ab/cd"), "");
    }
}
