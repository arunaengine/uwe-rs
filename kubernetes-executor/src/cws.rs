//! Tells a Common Workflow Scheduler (CWS) about a run, its processes and each task over its
//! REST API, so that it places the pods that name it as their scheduler.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use executor::Job;
use serde_json::{Value, json};
use tokio::sync::{Mutex, OnceCell};

/// How often the registration of the run is tried while CWS refuses connections, and the pause
/// between tries, as nf-cws waits for a scheduler that starts.
const TRIES: u32 = 50;
const PAUSE: Duration = Duration::from_secs(3);

/// How long one call may take.
const TIMEOUT: Duration = Duration::from_secs(60);

/// The settings of `[executor.kubernetes.cws]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    /// The address of CWS, like `http://cws.lab:8080`, without `/v1`.
    pub address: String,
    /// The scheduling strategy, like `rank_max-fair`.
    pub strategy: String,
    pub cost_function: Option<String>,
    /// The `schedulerName` of the pods: the `SCHEDULER_NAME` of the CWS instance.
    pub scheduler_name: String,
}

impl Settings {
    /// Settings with nf-cws's default strategy and the scheduler name of the CWS README.
    pub fn new(address: &str) -> Settings {
        Settings {
            address: address.trim_end_matches('/').to_owned(),
            strategy: "FIFO".into(),
            cost_function: None,
            scheduler_name: "workflow-scheduler".into(),
        }
    }
}

/// A client of CWS for one run, which it knows as the execution `run`.
#[derive(Debug)]
pub struct Client {
    pub settings: Settings,
    pub run: String,
    /// The pause between tries of the registration, shorter in tests.
    pub pause: Duration,
    registered: OnceCell<()>,
    /// The vertex of each process that CWS was told of.
    vertices: Mutex<BTreeMap<String, u64>>,
    tasks: AtomicU64,
}

impl Client {
    pub fn new(settings: Settings, run: &str) -> Client {
        Client {
            settings,
            run: run.to_owned(),
            pause: PAUSE,
            registered: OnceCell::new(),
            vertices: Mutex::new(BTreeMap::new()),
            tasks: AtomicU64::new(0),
        }
    }

    /// Registers the task of an attempt, whose pod comes from the Job `pod`, before the Job
    /// exists; the run and the process of the task first, once each.
    pub async fn task(
        &self,
        job: Job<'_>,
        pod: &str,
        namespace: &str,
        work_dir: &Path,
        copied: bool,
    ) -> Result<(), String> {
        self.registered
            .get_or_try_init(|| self.register(namespace))
            .await?;
        // An attempt is `<run>-<step>-<task>-<attempt>`, and CWS knows tasks by their process.
        let name = job
            .name
            .strip_prefix(&format!("{}-", self.run))
            .unwrap_or(job.name);
        let mut parts = name.rsplitn(3, '-');
        let attempt: u64 = parts.next().and_then(|part| part.parse().ok()).unwrap_or(1);
        let task = parts.next().unwrap_or_default();
        let process = parts.next().unwrap_or(name).to_owned();
        self.vertex(&process).await?;
        let mut size = 0;
        for input in &job.staging.inputs {
            let metadata = tokio::fs::metadata(&input.source).await;
            size += metadata.map_or(0, |metadata| metadata.len());
        }
        let id = self.tasks.fetch_add(1, Ordering::Relaxed) + 1;
        let inputs = json!({"booleanInputs": [], "numberInputs": [], "stringInputs": [],
                            "fileInputs": []});
        let body = json!({
            "runName": pod,
            "inputs": inputs,
            "schedulerParams": {},
            "name": format!("{process} ({task})"),
            "task": process,
            "stageInMode": if copied { "copy" } else { "symlink" },
            "cpus": job.placement.cpus.max(1),
            "memoryInBytes": job.placement.memory_bytes,
            "workDir": work_dir.to_string_lossy(),
            "repetition": attempt.saturating_sub(1),
            "inputSize": size,
        });
        (self.call("registering the task", &format!("task/{id}"), &body)).await
    }

    /// Registers the run, again while CWS refuses connections.
    async fn register(&self, namespace: &str) -> Result<(), String> {
        let mut body = json!({
            "dns": self.settings.address,
            "namespace": namespace,
            "strategy": self.settings.strategy,
        });
        if let Some(cost) = &self.settings.cost_function {
            body["costFunction"] = json!(cost);
        }
        let mut tries = 1;
        loop {
            match self.send("", &body).await {
                Ok(200) => return Ok(()),
                Ok(code) => {
                    return Err(format!(
                        "Got code: {code} from k8s scheduler while registering"
                    ));
                }
                Err(Unanswered::Refused) if tries < TRIES => {
                    tokio::time::sleep(self.pause).await;
                    tries += 1;
                }
                Err(_) => {
                    return Err(format!(
                        "Cannot connect to scheduler under {}",
                        self.settings.address
                    ));
                }
            }
        }
    }

    /// Adds the vertex of a process to the graph of the run, once.
    async fn vertex(&self, process: &str) -> Result<(), String> {
        let mut vertices = self.vertices.lock().await;
        if vertices.contains_key(process) {
            return Ok(());
        }
        let uid = vertices.len() as u64 + 1;
        let body = json!([{"label": process, "type": "PROCESS", "uid": uid}]);
        let action = "adding a vertex";
        self.call(action, "DAG/vertices", &body).await?;
        vertices.insert(process.to_owned(), uid);
        Ok(())
    }

    async fn call(&self, action: &str, path: &str, body: &Value) -> Result<(), String> {
        match self.send(path, body).await {
            Ok(200) => Ok(()),
            Ok(code) => Err(format!(
                "Got code: {code} from nextflow scheduler, while {action}"
            )),
            Err(Unanswered::Refused) => Err(format!("CWS refused the connection while {action}")),
            Err(Unanswered::Failed(error)) => {
                Err(format!("CWS did not answer while {action}: {error}"))
            }
        }
    }

    /// Posts `body` to `/v1/scheduler/<run>/<path>` and gives the status of the answer.
    async fn send(&self, path: &str, body: &Value) -> Result<u16, Unanswered> {
        let address = format!("{}/v1/scheduler/{}", self.settings.address, self.run);
        let address = match path {
            "" => address,
            path => format!("{address}/{path}"),
        };
        let client = executor::http::client().map_err(Unanswered::Failed)?;
        let answer = (client.post(address))
            .timeout(TIMEOUT)
            .header("content-type", "application/json")
            .body(body.to_string())
            .send()
            .await;
        match answer {
            Ok(answer) => Ok(answer.status().as_u16()),
            Err(error) if error.is_connect() => Err(Unanswered::Refused),
            Err(error) => Err(Unanswered::Failed(error.to_string())),
        }
    }
}

/// Why a call got no answer.
enum Unanswered {
    /// No connection: CWS may still start.
    Refused,
    Failed(String),
}
