//! Finds the Kubernetes Jobs that one run left behind, reports their state and stops them.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fs::{File, TryLockError};
use std::path::Path;

use executor::ExecutorError;
use executor::orphans::{Orphan, Outcome, journals, orphan, read};
use executor::poll::{CANCEL_GRACE, REQUEST_GRACE};
use k8s_openapi::api::batch::v1::Job;
use serde_json::Value;

use crate::backend::{Owned, Submitted, journal_at};
use crate::{KubernetesBackend, RUN};

impl KubernetesBackend {
    /// The Jobs that the run `run` left in the namespace of this backend, with what the
    /// cluster says of them. Changes nothing.
    pub async fn report_run(&self, run: &str) -> Result<Vec<Orphan>, ExecutorError> {
        let mut found = Vec::new();
        for root in journals(&self.scratch, run)? {
            let Some(journal) = read(&root) else { continue };
            found.push(match self.foreign(run, &journal) {
                Some(reason) => orphan(&root, name(&journal), Outcome::Unclear(reason.into())),
                None => self.inspect(&root, &journal).await.0,
            });
        }
        Ok(found)
    }

    /// Deletes the Jobs that the run `run` left, as recovery does, without their outputs. It
    /// keeps the Job of an attempt whose uwe process still runs, of another cluster or
    /// identity, and one that does not name the run.
    pub async fn stop_run(&self, run: &str) -> Result<Vec<Orphan>, ExecutorError> {
        let mut found = Vec::new();
        for root in journals(&self.scratch, run)? {
            let Some(journal) = read(&root) else { continue };
            let kept = |reason: &str| orphan(&root, name(&journal), Outcome::Kept(reason.into()));
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
            if journal["stopped"] != true && journal["uid"].as_str().is_none_or(str::is_empty) {
                // A submission whose answer was lost is resolved as recovery does it.
                match self.identify(&root, &journal).await {
                    Submitted::Found(uid) => journal["uid"] = serde_json::json!(uid),
                    Submitted::Absent => journal["stopped"] = serde_json::json!(true),
                    Submitted::Unclear(reason) => {
                        found.push(kept(reason));
                        continue;
                    }
                }
                journal_at(&root, &journal)?;
            }
            let (mut orphan, job) = self.inspect(&root, &journal).await;
            let ended = orphan.outcome == Outcome::Ended;
            let named = (job.as_ref())
                .and_then(|job| job.metadata.annotations.as_ref())
                .and_then(|annotations| annotations.get(RUN));
            orphan.outcome = match &orphan.outcome {
                Outcome::Unclear(reason) => Outcome::Kept(reason.clone()),
                _ if job.is_some() && named.map(String::as_str) != Some(run) => {
                    Outcome::Kept("the Job does not name the run".into())
                }
                _ => match self.settle_orphan(&root, &journal).await {
                    Ok(()) if ended => Outcome::Ended,
                    Ok(()) => Outcome::Canceled,
                    Err(error) => Outcome::Failed(error.to_string()),
                },
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
        let same = journal["backend"] == "Kubernetes"
            && journal["server"].as_str() == Some(self.cluster.server())
            && journal["namespace"].as_str() == Some(self.cluster.namespace())
            && journal["auth"].as_str() == self.cluster.identity();
        (!same).then_some("another cluster, namespace or identity owns the Job")
    }

    /// The Job of a journal, with the state that the cluster gives now, and the Job itself
    /// when the cluster still has the Job of the attempt.
    async fn inspect(&self, root: &Path, journal: &Value) -> (Orphan, Option<Job>) {
        let mut found = orphan(root, name(journal), Outcome::Ended);
        if journal["stopped"] == true {
            return (found, None);
        }
        let (Some(name), Some(uid)) = (name(journal), journal["uid"].as_str()) else {
            found.outcome = Outcome::Unclear("the cluster has not given the UID of the Job".into());
            return (found, None);
        };
        let answer = tokio::time::timeout(REQUEST_GRACE, self.cluster.jobs().get_opt(name)).await;
        let job = match answer {
            Ok(Ok(Some(job))) if job.metadata.uid.as_deref() == Some(uid) => job,
            // The Job of the attempt is gone; another one may have its name now.
            Ok(Ok(_)) => return (found, None),
            Ok(Err(error)) => {
                found.outcome = Outcome::Unclear(error.to_string());
                return (found, None);
            }
            Err(_) => {
                found.outcome = Outcome::Unclear("the cluster did not answer in time".into());
                return (found, None);
            }
        };
        let status = job.status.clone().unwrap_or_default();
        let condition = (status.conditions.iter().flatten())
            .find(|condition| condition.status == "True")
            .filter(|condition| matches!(condition.type_.as_str(), "Complete" | "Failed"));
        let state = match condition {
            Some(condition) => condition.type_.to_lowercase(),
            None if status.active.unwrap_or(0) > 0 => "active".into(),
            None => "pending".into(),
        };
        if condition.is_none() {
            found.outcome = Outcome::Running;
        }
        found.state = Some(state);
        (found, Some(job))
    }

    /// Deletes the Job of a journal unless it is gone, and removes its journal and its objects
    /// in the staging root.
    async fn settle_orphan(&self, root: &Path, journal: &Value) -> Result<(), ExecutorError> {
        Self::stop(&self.cluster, self.poll, root, journal, CANCEL_GRACE).await?;
        // No process waits for the outputs of the attempt any more.
        let mut journal = read(root).unwrap_or_else(|| journal.clone());
        journal["collect"] = serde_json::json!(false);
        Self::settle(self.remote.as_ref(), root, &journal).await
    }
}

/// The name of the Job of a journal.
fn name(journal: &Value) -> Option<&str> {
    journal["name"].as_str().filter(|name| !name.is_empty())
}
