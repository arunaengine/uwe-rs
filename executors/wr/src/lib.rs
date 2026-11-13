//! Runs task attempts as jobs of a wr manager, which uwe adds, watches and removes over wr's REST
//! API with the manager's token and CA certificate, with the job scripts of the SLURM jobs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod settings;

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use base64::Engine;
use executor::CONTROL;
use executor::wrapper::quote;
use serde_json::{Value, json};
use slurm_executor::grid::{GridBackend, Scheduler, State, Submission};

pub use settings::{Settings, deployment_name};

/// The wr executor's backend.
pub type WrBackend = GridBackend<Wr>;

/// Both streams of the job of an attempt, in its control folder.
pub const JOB_LOG: &str = "wr.log";

/// How long one call may take.
const TIMEOUT: Duration = Duration::from_secs(60);

/// How often uwe asks whether a job that it stopped is ready for removal, and how many times.
const STOP_PAUSE: Duration = Duration::from_millis(500);
const STOP_TRIES: u32 = 60;

/// A client of the REST API of one wr manager.
pub struct Wr {
    /// The address of the API, like `https://localhost:5022`.
    pub address: String,
    token: String,
    client: reqwest::Client,
}

impl std::fmt::Debug for Wr {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Wr")
            .field("address", &self.address)
            .finish_non_exhaustive()
    }
}

impl Wr {
    /// A client of the manager at `address` with the bearer `token`, which trusts only the CA
    /// certificates of the PEM text `ca`, and checks the host name as usual.
    pub fn new(address: &str, token: &str, ca: &[u8]) -> Result<Wr, String> {
        let certificates = reqwest::Certificate::from_pem_bundle(ca)
            .map_err(|error| format!("the CA certificate of wr cannot be read: {error}"))?;
        // Each user runs their own manager, which the web proxy does not reach.
        let client = reqwest::Client::builder()
            .tls_certs_only(certificates)
            .no_proxy()
            .connect_timeout(Duration::from_secs(30))
            .build()
            .map_err(|error| error.to_string())?;
        Ok(Wr {
            address: address.trim_end_matches('/').to_owned(),
            token: token.trim().to_owned(),
            client,
        })
    }

    /// A client of the manager that the settings name, with the token and CA certificate of
    /// the deployment in `home`, for the user `uid`.
    pub fn connect(settings: &Settings, home: &Path, uid: u32) -> Result<Wr, String> {
        let (token, ca) = settings.files(home);
        let read = |path: &Path, what: &str| {
            std::fs::read(path).map_err(|error| {
                format!(
                    "cannot read the {what} of wr at {}: {error}; start the manager with `wr \
                     manager start`, or set {what}_file in [executor.wr]",
                    path.display()
                )
            })
        };
        let text = read(&token, "token")?;
        let token = String::from_utf8(text).map_err(|_| "the token of wr is not text")?;
        Wr::new(&settings.address(uid), &token, &read(&ca, "ca")?)
    }

    /// Sends a call to `/rest/v1/<path>` and gives the status and the JSON of the answer.
    async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<&Value>,
    ) -> Result<(u16, Value), String> {
        let address = format!("{}/rest/v1/{path}", self.address);
        let url = match query.is_empty() {
            true => reqwest::Url::parse(&address),
            false => reqwest::Url::parse_with_params(&address, query),
        };
        let url = url.map_err(|error| error.to_string())?;
        let mut request = (self.client.request(method, url))
            .timeout(TIMEOUT)
            .bearer_auth(&self.token)
            .header("content-type", "application/json");
        if let Some(body) = body {
            request = request.body(body.to_string());
        }
        let answer =
            (request.send().await).map_err(|error| format!("no answer: {}", cause(&error)))?;
        let status = answer.status().as_u16();
        let text = (answer.text().await).map_err(|error| cause(&error))?;
        let value = serde_json::from_str(&text).unwrap_or(Value::String(text));
        Ok((status, value))
    }

    /// The state of a job; `None` for one that wr does not know.
    async fn state(&self, id: &str) -> Result<Option<String>, String> {
        let (status, answer) =
            (self.send(reqwest::Method::GET, &format!("jobs/{id}"), &[], None)).await?;
        match status {
            200 => Ok(answer[0]["State"].as_str().map(str::to_owned)),
            404 => Ok(None),
            _ => Err(format!("wr answered {status}: {answer}")),
        }
    }
}

impl Scheduler for Wr {
    const NAME: &'static str = "wr";
    const LOG: &'static str = JOB_LOG;

    async fn submit(&self, submission: &Submission<'_>) -> Result<String, String> {
        let body = json!([job(submission, command(submission).await?)]);
        let (status, answer) =
            (self.send(reqwest::Method::POST, "jobs/", &[], Some(&body))).await?;
        if status != 201 {
            return Err(format!("wr answered {status}: {answer}"));
        }
        (answer[0]["Key"].as_str().map(str::to_owned))
            .ok_or_else(|| format!("no job key in the answer of wr: {answer}"))
    }

    async fn states(&self, ids: &[String]) -> Result<HashMap<String, State>, String> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let path = format!("jobs/{}", ids.join(","));
        let (status, answer) = self.send(reqwest::Method::GET, &path, &[], None).await?;
        match status {
            200 => Ok(states(&answer)),
            404 => Ok(HashMap::new()),
            _ => Err(format!("wr answered {status}: {answer}")),
        }
    }

    /// Stops a running job, and then deletes it from the queue, so that wr keeps no job of
    /// uwe that failed or was cancelled.
    async fn remove(&self, id: &str) -> Result<(), String> {
        for _ in 0..STOP_TRIES {
            let mode = match self.state(id).await?.as_deref() {
                None | Some("complete" | "deleted") => return Ok(()),
                Some("running") => "running",
                Some("lost") => "lost",
                Some(_) => "deletable",
            };
            let path = format!("jobs/{id}");
            let query = [("state", mode)];
            let (status, answer) =
                (self.send(reqwest::Method::DELETE, &path, &query, None)).await?;
            if !matches!(status, 200 | 202) {
                return Err(format!("wr answered {status}: {answer}"));
            }
            if mode == "deletable" {
                return Ok(());
            }
            tokio::time::sleep(STOP_PAUSE).await;
        }
        Err(format!("the wr job {id} did not stop"))
    }
}

/// The command of the job of an attempt: its job script, with the log in the control folder;
/// or, on nodes that do not see the folders of uwe, the job script itself.
async fn command(submission: &Submission<'_>) -> Result<String, String> {
    let script = submission.script;
    if !submission.remote {
        let log = submission.root.join(CONTROL).join(JOB_LOG);
        let (script, log) = (script.to_string_lossy(), log.to_string_lossy());
        return Ok(format!(
            "/bin/bash {} > {} 2>&1",
            quote(&script),
            quote(&log)
        ));
    }
    let text = (tokio::fs::read(script).await).map_err(|error| error.to_string())?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(text);
    Ok(format!(
        "/bin/bash -c \"$(printf %s {encoded} | base64 -d)\" uwe"
    ))
}

/// The wr job of an attempt that runs `command`: its resources, which override what wr learned
/// of the group, no retries of wr's own, and a clean folder on nodes without the shared folders.
pub fn job(submission: &Submission<'_>, command: String) -> Value {
    let placement = submission.job.placement;
    let name = submission.job.name;
    // An attempt is `<run>-<step>-<task>-<attempt>`; wr learns resources by the group.
    let group = format!("[uwe] {}", name.rsplitn(3, '-').nth(2).unwrap_or(name));
    let remote = submission.remote;
    let given = placement.memory_bytes.is_some()
        || placement.time_seconds.is_some()
        || placement.disk_bytes.is_some();
    let mut job = json!({
        "cmd": command,
        "cwd": if remote { "/tmp".into() } else { submission.root.to_string_lossy() },
        "cwd_matters": !remote,
        "rep_grp": group,
        "req_grp": group,
        "override": if given { 2 } else { 0 },
        "retries": 0,
        "rerun": true,
        "cpus": placement.cpus.max(1),
    });
    if let Some(bytes) = placement.memory_bytes {
        job["memory"] = json!(format!("{}M", bytes.div_ceil(1 << 20)));
    }
    if let Some(seconds) = placement.time_seconds {
        job["time"] = json!(format!("{}s", seconds + submission.margin));
    }
    if let Some(bytes) = placement.disk_bytes {
        job["disk"] = json!(bytes.div_ceil(1 << 30));
    }
    if remote {
        job["on_exit"] = json!([{"cleanup": true}]);
    }
    job
}

/// The states of the jobs in an answer of `GET /rest/v1/jobs/<keys>`.
fn states(answer: &Value) -> HashMap<String, State> {
    let jobs = answer.as_array().map(Vec::as_slice).unwrap_or_default();
    let mut states = HashMap::new();
    for job in jobs {
        let Some(key) = job["Key"].as_str() else {
            continue;
        };
        let state = match job["State"].as_str().unwrap_or_default() {
            "complete" | "deleted" => {
                State::Ended(job["State"].as_str().unwrap_or_default().into())
            }
            "buried" => State::Ended(match job["FailReason"].as_str() {
                Some(reason) if !reason.is_empty() => format!("buried: {reason}"),
                _ => "buried".into(),
            }),
            _ => State::Active,
        };
        states.insert(key.to_owned(), state);
    }
    states
}

/// What went wrong with a request, in the words of its deepest cause.
fn cause(error: &reqwest::Error) -> String {
    let mut cause: &dyn std::error::Error = error;
    while let Some(source) = cause.source() {
        cause = source;
    }
    cause.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_job_states() {
        let answer = json!([
            {"Key": "a1", "State": "running"},
            {"Key": "b2", "State": "complete", "Exitcode": 0},
            {"Key": "c3", "State": "buried", "FailReason": "command exited non-zero"},
            {"Key": "d4", "State": "lost"},
        ]);
        let states = states(&answer);
        assert_eq!(states["a1"], State::Active);
        assert_eq!(states["b2"], State::Ended("complete".into()));
        let buried = State::Ended("buried: command exited non-zero".into());
        assert_eq!(states["c3"], buried);
        assert_eq!(states["d4"], State::Active);
        assert_eq!(states.len(), 4);
    }
}
