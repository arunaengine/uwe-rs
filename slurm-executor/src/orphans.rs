//! Finds the SLURM jobs that one run left behind, reports their state and cancels them.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fs::{File, TryLockError};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use executor::ExecutorError;
use executor::orphans::{Orphan, Outcome, journals, orphan, read};
use serde_json::Value;

use crate::SlurmBackend;
use crate::backend::{Owned, Submitted, comment, journal_at};
use crate::queue::{Described, described, ended};

impl SlurmBackend {
    /// The jobs that the run `run` left, with what `squeue` says of them. Changes nothing.
    pub async fn report_run(&self, run: &str) -> Result<Vec<Orphan>, ExecutorError> {
        let mut found = Vec::new();
        for root in journals(&self.scratch, run)? {
            let Some(journal) = read(&root) else { continue };
            found.push(match self.foreign(run, &journal) {
                Some(reason) => orphan(&root, id(&journal), Outcome::Unclear(reason.into())),
                None => self.inspect(&root, &journal).await.0,
            });
        }
        Ok(found)
    }

    /// Cancels the jobs that the run `run` left, as recovery does, without their outputs. It
    /// keeps the job of an attempt whose uwe process still runs, of another configuration or
    /// user, and one whose comment does not name the run.
    pub async fn stop_run(&self, run: &str) -> Result<Vec<Orphan>, ExecutorError> {
        let mut found = Vec::new();
        for root in journals(&self.scratch, run)? {
            let Some(journal) = read(&root) else { continue };
            let kept = |reason: &str| orphan(&root, id(&journal), Outcome::Kept(reason.into()));
            if let Some(reason) = self.foreign(run, &journal) {
                found.push(kept(reason));
                continue;
            }
            let lock = File::options().write(true).open(root.join("remote.lock"));
            let _owned = match lock.map(|lock| (lock.try_lock(), lock)) {
                Ok((Ok(()), lock)) => Owned(lock),
                Ok((Err(TryLockError::WouldBlock), _)) => {
                    found.push(kept("the uwe process of the attempt still runs"));
                    continue;
                }
                Ok((Err(TryLockError::Error(error)), _)) | Err(error) => {
                    found.push(kept(&format!("the journal cannot be locked: {error}")));
                    continue;
                }
            };
            // The process of the attempt may have changed the journal before it ended.
            let Some(mut journal) = read(&root) else {
                continue;
            };
            if journal["stopped"] != true && !journal["identity"].is_object() {
                // A submission whose answer was lost is resolved as recovery does it.
                match self.identify(&root, &journal).await {
                    Submitted::Found(job, identity) => {
                        journal["id"] = serde_json::json!(job.id);
                        journal["cluster"] = serde_json::json!(job.cluster);
                        journal["identity"] = identity;
                    }
                    Submitted::Absent => {
                        self.engine_stopped(&root, &journal).await?;
                        journal["stopped"] = serde_json::json!(true);
                    }
                    Submitted::Unclear(reason) => {
                        found.push(kept(reason));
                        continue;
                    }
                }
                journal_at(&root, &journal)?;
            }
            let (mut orphan, job) = self.inspect(&root, &journal).await;
            let owner = std::fs::metadata(&root).map(|folder| folder.uid()).ok();
            orphan.outcome = match (&orphan.outcome, job) {
                (Outcome::Unclear(reason), _) => Outcome::Kept(reason.clone()),
                (_, Some(job)) if job.comment != comment(run) => {
                    Outcome::Kept("the comment of the job does not name the run".into())
                }
                (_, Some(job)) if Some(job.user) != owner => {
                    Outcome::Kept("another user owns the job".into())
                }
                (outcome, _) => {
                    let ended = *outcome == Outcome::Ended;
                    match self.settle_orphan(&root, &journal).await {
                        Ok(()) if ended => Outcome::Ended,
                        Ok(()) => Outcome::Canceled,
                        Err(error) => Outcome::Failed(error.to_string()),
                    }
                }
            };
            found.push(orphan);
        }
        Ok(found)
    }

    /// Why the journal is not one that this backend may act on for the run `run`, if it is not.
    fn foreign(&self, run: &str, journal: &Value) -> Option<&'static str> {
        if journal["run"].as_str() != Some(run) {
            // The folder name alone cannot tell this run from a run whose id starts with it.
            return Some("the journal names no run, so it may belong to another");
        }
        if journal["backend"] != "SLURM" || journal["locked"] != true {
            return Some("the journal is not one of a SLURM job that uwe can take over");
        }
        (journal["configuration"] != self.configuration())
            .then_some("another SLURM configuration owns the job")
    }

    /// The job of a journal, with the state that `squeue` gives now, and what it says of the
    /// job while SLURM still lists it.
    async fn inspect(&self, root: &Path, journal: &Value) -> (Orphan, Option<Described>) {
        let mut found = orphan(root, id(journal), Outcome::Ended);
        if journal["stopped"] == true {
            return (found, None);
        }
        let Some(id) = id(journal) else {
            found.outcome = Outcome::Unclear("SLURM has not given the id of the job".into());
            return (found, None);
        };
        let squeue = self.command("squeue");
        let cluster = journal["cluster"].as_str();
        match described(&squeue, id, cluster, self.query_grace).await {
            Ok(Some(job)) => {
                if !ended(&job.state) {
                    found.outcome = Outcome::Running;
                }
                found.state = Some(job.state.clone());
                (found, Some(job))
            }
            Ok(None) => (found, None),
            Err(error) => {
                found.outcome = Outcome::Unclear(error);
                (found, None)
            }
        }
    }

    /// Cancels the job of a journal unless it ended, and removes its journal and its objects in
    /// the staging root.
    async fn settle_orphan(&self, root: &Path, journal: &Value) -> Result<(), ExecutorError> {
        if let Some(id) = id(journal) {
            self.stop(id, root, self.settle(), true).await?;
        }
        // No process waits for the outputs of the attempt any more.
        let mut journal = read(root).unwrap_or_else(|| journal.clone());
        journal["collect"] = serde_json::json!(false);
        self.settle_journal(root, &journal).await
    }
}

/// The id of the job of a journal.
fn id(journal: &Value) -> Option<&str> {
    journal["id"].as_str().filter(|id| !id.is_empty())
}
