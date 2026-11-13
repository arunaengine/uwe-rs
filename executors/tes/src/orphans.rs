//! Finds the TES tasks that one run left behind, reports their state and stops them.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use executor::ExecutorError;
pub use executor::orphans::{Orphan, Outcome, journals};
use executor::orphans::{held, read};
use executor::poll::REQUEST_GRACE;
use serde_json::Value;

use crate::TesBackend;
use crate::client::State;

/// Whose journal one is, as far as a call for a run can tell.
enum Owner<'a> {
    Backend(&'a TesBackend),
    /// A journal of this run or of no named run that no backend of `peers` may touch.
    Foreign(&'static str),
    /// A journal of another run.
    Other,
}

impl TesBackend {
    /// The tasks that the run `run` left on the TES servers, with what the servers of `peers`
    /// say of them. Changes nothing.
    pub async fn report_run(
        &self,
        run: &str,
        peers: &[&TesBackend],
    ) -> Result<Vec<Orphan>, ExecutorError> {
        let mut found = Vec::new();
        for root in journals(&self.scratch, run)? {
            let Some(journal) = read(&root) else { continue };
            let orphan = match owner(peers, run, &journal) {
                Owner::Other => continue,
                Owner::Foreign(reason) => orphan(&root, &journal, Outcome::Unclear(reason.into())),
                Owner::Backend(backend) => backend.inspect(&root, &journal).await,
            };
            found.push(orphan);
        }
        Ok(found)
    }

    /// Cancels the tasks that the run `run` left on the TES servers of `peers` and waits until
    /// the servers confirm their end, with the recovery that admission uses. It keeps an
    /// attempt whose uwe process still runs, and one whose server or token is not in `peers`.
    pub async fn stop_run(
        &self,
        run: &str,
        peers: &[&TesBackend],
    ) -> Result<Vec<Orphan>, ExecutorError> {
        let mut found = Vec::new();
        for root in journals(&self.scratch, run)? {
            let Some(journal) = read(&root) else { continue };
            let backend = match owner(peers, run, &journal) {
                Owner::Other => continue,
                Owner::Foreign(reason) => {
                    found.push(orphan(&root, &journal, Outcome::Kept(reason.into())));
                    continue;
                }
                Owner::Backend(backend) => backend,
            };
            let mut orphan = backend.inspect(&root, &journal).await;
            let ended = orphan.outcome == Outcome::Ended;
            orphan.outcome = match self.recover(peers, vec![root.clone()]).await {
                Err(error) => Outcome::Failed(backend.client.redact(&error.to_string())),
                Ok(()) if root.join("remote.json").try_exists()? => Outcome::Kept(kept(&root)),
                Ok(()) if ended => Outcome::Ended,
                Ok(()) => match &orphan.id {
                    Some(id) => {
                        let status = tokio::time::timeout(REQUEST_GRACE, backend.client.status(id));
                        if let Ok(Ok(status)) = status.await {
                            orphan.state = named(status.state);
                        }
                        Outcome::Canceled
                    }
                    None => Outcome::Ended,
                },
            };
            found.push(orphan);
        }
        Ok(found)
    }

    /// The task of a journal, with the state that the server gives now.
    async fn inspect(&self, root: &Path, journal: &Value) -> Orphan {
        if journal["stopped"] == true {
            let mut found = orphan(root, journal, Outcome::Ended);
            let state = serde_json::from_value(journal["status"]["state"].clone()).ok();
            found.state = state.and_then(named);
            return found;
        }
        let mut found = orphan(root, journal, Outcome::Unclear(String::new()));
        let Some(id) = found.id.clone() else {
            found.outcome = Outcome::Unclear("the server has not given the id of the task".into());
            return found;
        };
        found.outcome = match tokio::time::timeout(REQUEST_GRACE, self.client.status(&id)).await {
            Ok(Ok(status)) => {
                found.state = named(status.state);
                match status.state.is_final() {
                    true => Outcome::Ended,
                    false => Outcome::Running,
                }
            }
            Ok(Err(error)) => Outcome::Unclear(self.client.redact(&error.to_string())),
            Err(_) => Outcome::Unclear("the TES server did not answer in time".into()),
        };
        found
    }
}

/// The backend of `peers` that owns a journal of the run `run`, as recovery picks it.
fn owner<'a>(peers: &[&'a TesBackend], run: &str, journal: &Value) -> Owner<'a> {
    match journal["run"].as_str() {
        Some(named) if named == run => {}
        Some(_) => return Owner::Other,
        // The folder name alone cannot tell this run from a run whose id starts with it.
        None => return Owner::Foreign("the journal names no run, so it may belong to another"),
    }
    let found = peers.iter().find(|peer| {
        Some(peer.client.server()) == journal["server"].as_str()
            && (journal["auth"].as_str()).is_none_or(|auth| auth == peer.client.identity())
    });
    match found {
        Some(backend) => Owner::Backend(backend),
        None => Owner::Foreign("another TES server or token owns the task"),
    }
}

fn orphan(root: &Path, journal: &Value, outcome: Outcome) -> Orphan {
    executor::orphans::orphan(root, journal["id"].as_str(), outcome)
}

/// The name of a state as the TES server gives it, like `RUNNING`.
fn named(state: State) -> Option<String> {
    serde_json::to_value(state)
        .ok()?
        .as_str()
        .map(str::to_owned)
}

/// Why recovery left the journal in `root`.
fn kept(root: &Path) -> String {
    if held(root) {
        return "the uwe process of the attempt still runs".into();
    }
    match read(root).is_some_and(|journal| journal["id"].as_str().is_none_or(str::is_empty)) {
        true => "the submission of the task is not clear yet".into(),
        false => "the journal of the attempt cannot be settled yet".into(),
    }
}

#[cfg(test)]
#[path = "orphans_tests.rs"]
mod tests;
