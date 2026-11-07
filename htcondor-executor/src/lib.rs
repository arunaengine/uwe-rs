//! Runs task attempts as HTCondor jobs, which uwe submits, watches and removes with the HTCondor
//! tools on an access point.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod submit;

use std::collections::HashMap;
use std::path::PathBuf;

use slurm_executor::executable;
use slurm_executor::grid::{GridBackend, Scheduler, State, Submission, call};
use tokio::process::Command;

pub use submit::{EVENT_LOG, JOB_LOG, SUBMIT_FILE, description};

/// The HTCondor tools that the backend runs. Runs need the first three; `condor_history` only
/// tells how a job ended that left the queue.
pub const TOOLS: [&str; 4] = ["condor_submit", "condor_q", "condor_rm", "condor_history"];

/// The HTCondor executor's backend.
pub type HtcondorBackend = GridBackend<Condor>;

/// The HTCondor scheduler of an access point.
#[derive(Clone, Debug, Default)]
pub struct Condor {
    /// The folder of the HTCondor tools, when they are not on the PATH.
    pub commands: Option<PathBuf>,
    /// More lines of every submit description, like `+ProjectName = "lab"`.
    pub lines: Vec<String>,
}

impl Condor {
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

    /// How a job that left the queue ended, from the history of the schedd.
    async fn history(&self, id: &str) -> Option<State> {
        let tool = self.path("condor_history");
        if !executable(&tool) {
            return None;
        }
        let mut command = Command::new(tool);
        command.args([
            id,
            "-limit",
            "1",
            "-af",
            "JobStatus",
            "ExitCode",
            "RemoveReason",
        ]);
        let output = call(&mut command).await.ok()?;
        let text = String::from_utf8_lossy(&output.stdout);
        let mut words = text.lines().next()?.split_whitespace();
        let (status, code) = (words.next()?, words.next()?);
        let reason = words.collect::<Vec<_>>().join(" ");
        match status {
            "4" => Some(State::Ended(format!("completed with exit code {code}"))),
            "3" => Some(State::Ended(format!("removed ({reason})"))),
            _ => None,
        }
    }
}

impl Scheduler for Condor {
    const NAME: &'static str = "HTCondor";
    const LOG: &'static str = JOB_LOG;

    async fn submit(&self, submission: &Submission<'_>) -> Result<String, String> {
        let file = submission.script.with_file_name(SUBMIT_FILE);
        let text = description(submission, &self.lines);
        (tokio::fs::write(&file, text).await)
            .map_err(|error| format!("cannot write {}: {error}", file.display()))?;
        let mut command = Command::new(self.path("condor_submit"));
        command.arg("-terse").arg(&file);
        let output = call(&mut command).await?;
        // `-terse` prints the first and last job of the cluster, like `1001.0 - 1001.0`.
        let text = String::from_utf8_lossy(&output.stdout);
        let id = text.split_whitespace().next().unwrap_or_default();
        match output.status.success() && job_id(id) {
            true => Ok(id.to_owned()),
            false => Err(String::from_utf8_lossy(&output.stderr).trim().to_owned()),
        }
    }

    async fn states(&self, ids: &[String]) -> Result<HashMap<String, State>, String> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let mut command = Command::new(self.path("condor_q"));
        command
            .args(ids)
            .args(["-af", "ClusterId", "ProcId", "JobStatus", "HoldReason"]);
        let output = call(&mut command).await?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
        }
        let mut states = queued(&String::from_utf8_lossy(&output.stdout));
        for id in ids {
            if !states.contains_key(id)
                && let Some(state) = self.history(id).await
            {
                states.insert(id.clone(), state);
            }
        }
        Ok(states)
    }

    async fn remove(&self, id: &str) -> Result<(), String> {
        let output = call(Command::new(self.path("condor_rm")).arg(id)).await?;
        match output.status.success() {
            true => Ok(()),
            false => Err(String::from_utf8_lossy(&output.stderr).trim().to_owned()),
        }
    }
}

/// Whether a text is a job id, `<cluster>.<proc>`.
fn job_id(text: &str) -> bool {
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    text.split_once('.')
        .is_some_and(|(cluster, proc)| digits(cluster) && digits(proc))
}

/// The states of the jobs in the lines of `condor_q -af ClusterId ProcId JobStatus HoldReason`,
/// by HTCondor's numbers: 1 idle, 2 running, 3 removed, 4 completed, 5 held, 6 transferring
/// output, 7 suspended.
fn queued(text: &str) -> HashMap<String, State> {
    let mut states = HashMap::new();
    for line in text.lines() {
        let mut words = line.split_whitespace();
        let (Some(cluster), Some(proc), Some(status)) = (words.next(), words.next(), words.next())
        else {
            continue;
        };
        let reason = words.collect::<Vec<_>>().join(" ");
        let state = match status {
            "3" => State::Ended("removed".into()),
            "4" => State::Ended("completed".into()),
            "5" => State::Held(reason),
            _ => State::Active,
        };
        states.insert(format!("{cluster}.{proc}"), state);
    }
    states
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_queue_lines() {
        let text = "1001 0 2 undefined\n1002 0 5 Error from slot1@node: out of disk\n\
                    1003 1 4 undefined\n";
        let states = queued(text);
        assert_eq!(states["1001.0"], State::Active);
        let held = State::Held("Error from slot1@node: out of disk".into());
        assert_eq!(states["1002.0"], held);
        assert_eq!(states["1003.1"], State::Ended("completed".into()));
        assert!(job_id("1001.0"));
        assert!(!job_id("Submitting"));
        assert!(!job_id("1001."));
    }
}
