//! Runs task attempts as PBS Pro and OpenPBS jobs, which uwe submits, watches and removes with
//! `qsub`, `qstat` and `qdel` on a login node.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod head;
mod summary;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use executor::CONTROL;
use executor::wrapper::STDERR;
use serde_json::Value;
use slurm_executor::grid::{GridBackend, Scheduler, State, Submission, call};
use slurm_executor::{LOG, executable};
use tokio::process::Command;

pub use head::head;
pub use summary::summary;

/// The PBS tools that the backend runs.
pub const TOOLS: [&str; 3] = ["qsub", "qstat", "qdel"];

/// The PBS executor's backend.
pub type PbsBackend = GridBackend<Pbs>;

/// The variables of the web proxy, which `qsub -v` hands to the jobs.
const PROXY: [&str; 4] = ["HTTPS_PROXY", "HTTP_PROXY", "https_proxy", "http_proxy"];

/// A PBS Pro or OpenPBS server, and what its jobs ask for besides their resources.
#[derive(Clone, Debug, Default)]
pub struct Pbs {
    /// The folder of the PBS tools, when they are not on the PATH.
    pub commands: Option<PathBuf>,
    /// More options of `qsub`, like `-A lab`, which come on its command line and so win.
    pub options: Vec<String>,
    /// The project of the jobs, `-P`, as NCI Gadi needs it.
    pub project: Option<String>,
    /// The `storage` resource of the jobs, like `gdata/ab12+scratch/ab12` on Gadi.
    pub storage: Option<String>,
    /// The resource that gets the disk of a task, like Gadi's `jobfs`; `None` leaves it out.
    pub disk_resource: Option<String>,
}

impl Pbs {
    /// The tools that are not there: not in the folder of `commands`, or not on the PATH.
    pub fn missing_tools(&self) -> Vec<&'static str> {
        TOOLS
            .into_iter()
            .filter(|tool| !executable(&self.path(tool)))
            .collect()
    }

    fn path(&self, name: &str) -> PathBuf {
        match &self.commands {
            Some(folder) => folder.join(name),
            None => PathBuf::from(name),
        }
    }
}

impl Scheduler for Pbs {
    const NAME: &'static str = "PBS";

    fn head(&self, submission: &Submission<'_>) -> String {
        head(submission, self)
    }

    async fn submit(&self, submission: &Submission<'_>) -> Result<String, String> {
        let mut command = Command::new(self.path("qsub"));
        if let Some(proxy) = submission.proxy {
            for name in PROXY {
                command.env(name, proxy);
            }
            command.arg("-v").arg(PROXY.join(","));
        }
        command.args(&self.options).arg(submission.script);
        let output = call(&mut command).await?;
        let text = String::from_utf8_lossy(&output.stdout);
        let id = text.trim();
        let number = id.split('.').next().unwrap_or_default();
        match output.status.success()
            && !number.is_empty()
            && number.bytes().all(|byte| byte.is_ascii_digit())
        {
            true => Ok(id.to_owned()),
            false => Err(String::from_utf8_lossy(&output.stderr).trim().to_owned()),
        }
    }

    async fn states(&self, ids: &[String]) -> Result<HashMap<String, State>, String> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let mut command = Command::new(self.path("qstat"));
        command.args(["-x", "-f", "-F", "json"]).args(ids);
        let output = call(&mut command).await?;
        // Unknown jobs fail the call, but the others are still in the answer.
        match serde_json::from_slice::<Value>(&output.stdout) {
            Ok(answer) => Ok(states(&answer, ids)),
            Err(_) => {
                let error = String::from_utf8_lossy(&output.stderr);
                let unknown = error.lines().all(|line| line.contains("Unknown Job Id"));
                match unknown && !error.trim().is_empty() {
                    true => Ok(HashMap::new()),
                    false => Err(error.trim().to_owned()),
                }
            }
        }
    }

    async fn remove(&self, id: &str) -> Result<(), String> {
        let output = call(Command::new(self.path("qdel")).arg(id)).await?;
        let error = String::from_utf8_lossy(&output.stderr);
        // A job that ended meanwhile is gone already.
        match output.status.success() || error.contains("Unknown Job Id") {
            true => Ok(()),
            false => Err(error.trim().to_owned()),
        }
    }

    /// Adds the resource summary that sites like Gadi write at the end of the job log to the
    /// standard error of the task, where reports read it.
    fn ended(&self, root: &Path) -> std::io::Result<()> {
        use std::io::Write;
        let control = root.join(CONTROL);
        let Ok(log) = std::fs::read_to_string(control.join(LOG)) else {
            return Ok(());
        };
        let Some(block) = summary(&log) else {
            return Ok(());
        };
        let mut file =
            (std::fs::OpenOptions::new().create(true).append(true)).open(control.join(STDERR))?;
        file.write_all(block.as_bytes())
    }
}

/// The states of the jobs `ids` in the answer of `qstat -f -F json`, which may name a job
/// with a longer server name than `qsub` gave.
fn states(answer: &Value, ids: &[String]) -> HashMap<String, State> {
    let Some(jobs) = answer["Jobs"].as_object() else {
        return HashMap::new();
    };
    let number = |id: &str| id.split('.').next().unwrap_or_default().to_owned();
    let mut states = HashMap::new();
    for id in ids {
        let job = (jobs.get(id)).or_else(|| {
            (jobs.iter()).find_map(|(key, job)| (number(key) == number(id)).then_some(job))
        });
        let Some(job) = job else {
            continue;
        };
        let text = |key: &str| match &job[key] {
            Value::String(text) => text.clone(),
            Value::Null => String::new(),
            other => other.to_string(),
        };
        let state = match job["job_state"].as_str().unwrap_or_default() {
            "H" => State::Held(match text("comment") {
                comment if comment.is_empty() => "held".to_owned(),
                comment => comment,
            }),
            "F" | "X" => State::Ended(format!("finished with exit status {}", text("Exit_status"))),
            _ => State::Active,
        };
        states.insert(id.clone(), state);
    }
    states
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_job_states() {
        let answer = serde_json::json!({"pbs_version": "2024.1", "Jobs": {
            "11.server.example.org": {"job_state": "R"},
            "12.server": {"job_state": "H", "comment": "job held, too many failed attempts"},
            "13.server": {"job_state": "F", "Exit_status": 271},
        }});
        let ids: Vec<String> = ["11.server", "12.server", "13.server", "14.server"]
            .map(str::to_owned)
            .into();
        let states = states(&answer, &ids);
        assert_eq!(states["11.server"], State::Active);
        let held = State::Held("job held, too many failed attempts".into());
        assert_eq!(states["12.server"], held);
        let ended = State::Ended("finished with exit status 271".into());
        assert_eq!(states["13.server"], ended);
        assert!(!states.contains_key("14.server"));
    }
}
