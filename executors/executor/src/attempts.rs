//! Keeps the journals of remote attempts, by which a later process finds and stops their jobs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, TryLockError};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Value, json};

use crate::ExecutorError;
use crate::orphans::RemoteJobs;

/// Where remote executors keep the journal of each attempt while its job may run: the run, the
/// id of the job on its server, and what else the executor needs to find it again. The default
/// [`FileJournal`] keeps them on this machine; a host may keep them in its own database.
pub trait JournalStore: Send + Sync {
    /// Writes the journal of an attempt, replacing the one before.
    fn save(&self, executor: &str, attempt: &str, journal: &Value) -> Result<(), ExecutorError>;
    fn load(&self, executor: &str, attempt: &str) -> Result<Option<Value>, ExecutorError>;
    /// The attempts of the run `run` that have a journal, by name.
    fn list(&self, executor: &str, run: &str) -> Result<Vec<String>, ExecutorError>;
    /// Holds the attempt for this process until the lock drops; `None` when another process
    /// holds it, like the process that runs the attempt.
    fn acquire(&self, executor: &str, attempt: &str) -> Result<Option<JournalLock>, ExecutorError>;
    /// Forgets the journal of an attempt whose job ended; a missing journal is no error.
    fn remove(&self, executor: &str, attempt: &str) -> Result<(), ExecutorError>;
}

/// Holds an attempt of a [`JournalStore`] until it is dropped.
pub struct JournalLock {
    _held: Box<dyn Send + Sync>,
}

impl JournalLock {
    /// A lock that gives the attempt back when `held` drops.
    pub fn new(held: impl Send + Sync + 'static) -> JournalLock {
        JournalLock {
            _held: Box::new(held),
        }
    }
}

impl std::fmt::Debug for JournalLock {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("JournalLock")
    }
}

/// The journals in folders of this machine, as the executors always kept them:
/// `<root>/<executor>/<attempt>/remote.json`, held with a lock of `remote.lock` beside it.
#[derive(Clone, Debug)]
pub struct FileJournal {
    pub root: PathBuf,
}

impl FileJournal {
    pub fn new(root: impl Into<PathBuf>) -> FileJournal {
        FileJournal { root: root.into() }
    }

    fn folder(&self, executor: &str, attempt: &str) -> Result<PathBuf, ExecutorError> {
        for name in [executor, attempt] {
            if name.is_empty() || name.contains(['/', '\0']) || name.starts_with('.') {
                return Err(ExecutorError::Unsupported(format!(
                    "a journal cannot be named {name:?}"
                )));
            }
        }
        Ok(self.root.join(executor).join(attempt))
    }
}

impl JournalStore for FileJournal {
    fn save(&self, executor: &str, attempt: &str, journal: &Value) -> Result<(), ExecutorError> {
        let folder = self.folder(executor, attempt)?;
        std::fs::create_dir_all(&folder)?;
        // A crash while writing must leave the journal before, never half of one.
        let next = folder.join("remote.json.next");
        std::fs::write(
            &next,
            serde_json::to_vec(journal).map_err(std::io::Error::other)?,
        )?;
        File::open(&next)?.sync_all()?;
        std::fs::rename(next, folder.join("remote.json"))?;
        // The rename is durable only once its folder is: a crash must not lose a journal.
        File::open(&folder)?.sync_all()?;
        Ok(())
    }

    fn load(&self, executor: &str, attempt: &str) -> Result<Option<Value>, ExecutorError> {
        Ok(crate::orphans::read(&self.folder(executor, attempt)?))
    }

    fn list(&self, executor: &str, run: &str) -> Result<Vec<String>, ExecutorError> {
        let found = crate::orphans::journals(&self.root.join(executor), run)?;
        Ok((found.iter())
            .filter_map(|root| Some(root.file_name()?.to_string_lossy().into_owned()))
            .collect())
    }

    fn acquire(&self, executor: &str, attempt: &str) -> Result<Option<JournalLock>, ExecutorError> {
        let folder = self.folder(executor, attempt)?;
        std::fs::create_dir_all(&folder)?;
        let lock = File::options()
            .write(true)
            .create(true)
            .truncate(false)
            .open(folder.join("remote.lock"))?;
        match lock.try_lock() {
            Ok(()) => Ok(Some(JournalLock::new(lock))),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(error)) => Err(error.into()),
        }
    }

    fn remove(&self, executor: &str, attempt: &str) -> Result<(), ExecutorError> {
        let path = self.folder(executor, attempt)?.join("remote.json");
        match std::fs::remove_file(path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
            _ => Ok(()),
        }
    }
}

/// The journals in the memory of this process, for tests and hosts that run no second process.
#[derive(Clone, Debug, Default)]
pub struct MemoryJournal {
    inner: Arc<Mutex<Memory>>,
}

#[derive(Debug, Default)]
struct Memory {
    journals: BTreeMap<(String, String), Value>,
    held: BTreeSet<(String, String)>,
}

/// Gives the attempt of a [`MemoryJournal`] back when dropped.
struct MemoryHold {
    inner: Arc<Mutex<Memory>>,
    key: (String, String),
}

impl Drop for MemoryHold {
    fn drop(&mut self) {
        lock(&self.inner).held.remove(&self.key);
    }
}

impl MemoryJournal {
    pub fn new() -> MemoryJournal {
        MemoryJournal::default()
    }
}

fn lock(memory: &Mutex<Memory>) -> std::sync::MutexGuard<'_, Memory> {
    memory.lock().unwrap_or_else(PoisonError::into_inner)
}

fn key(executor: &str, attempt: &str) -> (String, String) {
    (executor.to_owned(), attempt.to_owned())
}

impl JournalStore for MemoryJournal {
    fn save(&self, executor: &str, attempt: &str, journal: &Value) -> Result<(), ExecutorError> {
        let journals = &mut lock(&self.inner).journals;
        journals.insert(key(executor, attempt), journal.clone());
        Ok(())
    }

    fn load(&self, executor: &str, attempt: &str) -> Result<Option<Value>, ExecutorError> {
        Ok(lock(&self.inner)
            .journals
            .get(&key(executor, attempt))
            .cloned())
    }

    fn list(&self, executor: &str, run: &str) -> Result<Vec<String>, ExecutorError> {
        let prefix = format!("{run}-");
        let memory = lock(&self.inner);
        Ok((memory.journals.iter())
            .filter(|((owner, attempt), journal)| {
                owner == executor
                    && attempt.starts_with(&prefix)
                    && (journal["run"].is_null() || journal["run"] == run)
            })
            .map(|((_, attempt), _)| attempt.clone())
            .collect())
    }

    fn acquire(&self, executor: &str, attempt: &str) -> Result<Option<JournalLock>, ExecutorError> {
        let key = key(executor, attempt);
        if !lock(&self.inner).held.insert(key.clone()) {
            return Ok(None);
        }
        let inner = self.inner.clone();
        Ok(Some(JournalLock::new(MemoryHold { inner, key })))
    }

    fn remove(&self, executor: &str, attempt: &str) -> Result<(), ExecutorError> {
        lock(&self.inner).journals.remove(&key(executor, attempt));
        Ok(())
    }
}

/// The journals of one executor for the attempts of one run, as a backend writes them.
#[derive(Clone)]
pub struct Journals {
    pub store: Arc<dyn JournalStore>,
    /// The executor, like `aws-batch`, which names the jobs that the journals point to.
    pub executor: String,
    pub run: String,
}

impl std::fmt::Debug for Journals {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        (formatter.debug_struct("Journals"))
            .field("executor", &self.executor)
            .field("run", &self.run)
            .finish_non_exhaustive()
    }
}

impl Journals {
    pub fn new(store: Arc<dyn JournalStore>, executor: &str, run: &str) -> Journals {
        Journals {
            store,
            executor: executor.to_owned(),
            run: run.to_owned(),
        }
    }

    /// Holds an attempt before its job is submitted, so that no other process stops the job
    /// while this one waits for it.
    pub fn hold(&self, attempt: &str) -> Result<JournalLock, ExecutorError> {
        self.store
            .acquire(&self.executor, attempt)?
            .ok_or_else(|| ExecutorError::Unsupported(format!("{attempt} is held elsewhere")))
    }

    /// Notes that the job of an attempt is about to be submitted to the scheduler `scope`,
    /// whose id is not known yet.
    pub fn intended(&self, attempt: &str, scope: &str) -> Result<(), ExecutorError> {
        let journal = json!({
            "run": self.run,
            "backend": self.executor,
            "scope": scope,
            "id": null,
        });
        self.store.save(&self.executor, attempt, &journal)
    }

    /// Notes the job `id` of an attempt, with `details` that the executor finds it again by.
    pub fn submitted(&self, attempt: &str, id: &str, details: Value) -> Result<(), ExecutorError> {
        let mut journal = json!({
            "run": self.run,
            "backend": self.executor,
            "id": id,
        });
        if let (Some(journal), Value::Object(details)) = (journal.as_object_mut(), details) {
            journal.extend(details);
        }
        self.store.save(&self.executor, attempt, &journal)
    }

    /// Forgets an attempt whose job ended; a journal that stays is found by a later report.
    pub fn ended(&self, attempt: &str) {
        if let Err(error) = self.store.remove(&self.executor, attempt) {
            tracing::warn!(attempt, %error, "cannot remove the journal of an ended job");
        }
    }
}

/// Holds an attempt in `journals`, when the backend keeps any, and notes durably that its job
/// is about to go to the scheduler of `jobs`, so that a crash during the submission leaves a
/// journal. A journal that cannot be written stops the attempt before its job exists.
pub fn hold(
    journals: Option<&Journals>,
    jobs: &dyn RemoteJobs,
    attempt: &str,
) -> Result<Option<JournalLock>, ExecutorError> {
    let Some(journals) = journals else {
        return Ok(None);
    };
    let held = journals.hold(attempt)?;
    journals.intended(attempt, &jobs.scope())?;
    Ok(Some(held))
}

/// Notes the job `id` of an attempt in `journals`, when the backend keeps any, with the
/// scheduler that runs it. A job whose journal cannot be written would run untracked, so it is
/// cancelled, and the attempt fails.
pub async fn note(
    journals: Option<&Journals>,
    jobs: &dyn RemoteJobs,
    attempt: &str,
    id: &str,
) -> Result<(), ExecutorError> {
    let Some(journals) = journals else {
        return Ok(());
    };
    let details = json!({"scope": jobs.scope()});
    if let Err(error) = journals.submitted(attempt, id, details) {
        if let Err(cancel) = jobs.cancel(id).await {
            tracing::warn!(attempt, job = id, %cancel, "cannot stop an untracked job");
        }
        return Err(error);
    }
    Ok(())
}

/// Forgets an attempt in `journals` once waiting for its job ended it: it ended, or the server
/// confirms that the stop of a cancelled one ended it. A job whose stop failed or is not
/// confirmed, or that the wait lost touch with, keeps its journal for a later stop.
pub async fn settle<T>(
    journals: Option<&Journals>,
    jobs: &dyn RemoteJobs,
    attempt: &str,
    id: &str,
    ended: &Result<T, ExecutorError>,
) {
    let Some(journals) = journals else { return };
    let settled = match ended {
        Ok(_) => true,
        Err(ExecutorError::Cancelled) => crate::orphans::confirmed(jobs, id).await,
        Err(_) => false,
    };
    if settled {
        journals.ended(attempt);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(store: &dyn JournalStore) {
        let journal = json!({"run": "run", "id": "job-1"});
        store.save("aws-batch", "run-1-1", &journal).unwrap();
        store
            .save("aws-batch", "other-1-1", &json!({"run": "other"}))
            .unwrap();
        store
            .save("nomad", "run-2-1", &json!({"run": "run"}))
            .unwrap();
        assert_eq!(store.list("aws-batch", "run").unwrap(), ["run-1-1"]);
        assert_eq!(store.load("aws-batch", "run-1-1").unwrap(), Some(journal));
        let held = store.acquire("aws-batch", "run-1-1").unwrap();
        assert!(held.is_some());
        drop(held);
        assert!(store.acquire("aws-batch", "run-1-1").unwrap().is_some());
        store.remove("aws-batch", "run-1-1").unwrap();
        store.remove("aws-batch", "run-1-1").unwrap();
        assert!(store.list("aws-batch", "run").unwrap().is_empty());
        assert_eq!(store.load("aws-batch", "run-1-1").unwrap(), None);
    }

    #[test]
    fn keeps_journals() {
        let folder = tempfile::tempdir().unwrap();
        let files = FileJournal::new(folder.path());
        check(&files);
        let path = folder.path().join("nomad/run-2-1/remote.json");
        assert!(path.is_file(), "the format of the executors stays");
        assert!(files.save("../x", "a", &json!({})).is_err());
        check(&MemoryJournal::new());
    }

    /// A scheduler whose jobs are gone, or whose state cannot be read, that notes its calls.
    struct Scheduler {
        scope: &'static str,
        gone: bool,
        calls: Mutex<Vec<String>>,
    }

    impl RemoteJobs for Scheduler {
        fn state<'a>(
            &'a self,
            id: &'a str,
        ) -> futures_util::future::BoxFuture<'a, Result<Option<crate::orphans::JobState>, String>>
        {
            lock_calls(&self.calls).push(format!("state {id}"));
            Box::pin(async move {
                match self.gone {
                    true => Ok(None),
                    false => Err("the scheduler does not answer".into()),
                }
            })
        }

        fn cancel<'a>(
            &'a self,
            id: &'a str,
        ) -> futures_util::future::BoxFuture<'a, Result<(), String>> {
            lock_calls(&self.calls).push(format!("cancel {id}"));
            Box::pin(async { Ok(()) })
        }

        fn scope(&self) -> String {
            self.scope.to_owned()
        }
    }

    fn lock_calls(calls: &Mutex<Vec<String>>) -> std::sync::MutexGuard<'_, Vec<String>> {
        calls.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn scheduler(scope: &'static str, gone: bool) -> Scheduler {
        Scheduler {
            scope,
            gone,
            calls: Mutex::default(),
        }
    }

    #[tokio::test]
    async fn keeps_unconfirmed_cancels() {
        let memory = MemoryJournal::new();
        let journals = Journals::new(Arc::new(memory.clone()), "pbs", "run");
        let (answering, silent) = (scheduler("a", true), scheduler("a", false));
        for jobs in [&silent, &answering] {
            let held = hold(Some(&journals), jobs, "run-1-1").unwrap();
            let intent = memory.load("pbs", "run-1-1").unwrap().unwrap();
            assert_eq!(intent["id"], Value::Null, "the intent comes before the job");
            note(Some(&journals), jobs, "run-1-1", "17").await.unwrap();
            drop(held);
            let cancelled: Result<(), ExecutorError> = Err(ExecutorError::Cancelled);
            settle(Some(&journals), jobs, "run-1-1", "17", &cancelled).await;
        }
        // The silent scheduler did not confirm the stop, so its journal stayed until the next.
        assert_eq!(lock_calls(&silent.calls).as_slice(), ["state 17"]);
        assert!(memory.load("pbs", "run-1-1").unwrap().is_none());
        let failed: Result<(), ExecutorError> = Err(ExecutorError::Engine {
            engine: "PBS".into(),
            message: "lost touch".into(),
        });
        journals
            .submitted("run-1-1", "18", json!({"scope": "a"}))
            .unwrap();
        settle(Some(&journals), &answering, "run-1-1", "18", &failed).await;
        assert!(memory.load("pbs", "run-1-1").unwrap().is_some());
    }

    /// A store that keeps the intent of an attempt but cannot write the id of its job.
    struct Full(MemoryJournal);

    impl JournalStore for Full {
        fn save(
            &self,
            executor: &str,
            attempt: &str,
            journal: &Value,
        ) -> Result<(), ExecutorError> {
            match journal["id"].is_null() {
                true => self.0.save(executor, attempt, journal),
                false => Err(std::io::Error::other("the disk is full").into()),
            }
        }
        fn load(&self, executor: &str, attempt: &str) -> Result<Option<Value>, ExecutorError> {
            self.0.load(executor, attempt)
        }
        fn list(&self, executor: &str, run: &str) -> Result<Vec<String>, ExecutorError> {
            self.0.list(executor, run)
        }
        fn acquire(
            &self,
            executor: &str,
            attempt: &str,
        ) -> Result<Option<JournalLock>, ExecutorError> {
            self.0.acquire(executor, attempt)
        }
        fn remove(&self, executor: &str, attempt: &str) -> Result<(), ExecutorError> {
            self.0.remove(executor, attempt)
        }
    }

    #[tokio::test]
    async fn cancels_untracked_jobs() {
        let journals = Journals::new(Arc::new(Full(MemoryJournal::new())), "pbs", "run");
        let jobs = scheduler("a", true);
        let _held = hold(Some(&journals), &jobs, "run-1-1").unwrap();
        assert!(note(Some(&journals), &jobs, "run-1-1", "17").await.is_err());
        assert_eq!(lock_calls(&jobs.calls).as_slice(), ["cancel 17"]);
    }

    #[tokio::test]
    async fn skips_other_schedulers() {
        let memory = MemoryJournal::new();
        let journals = Journals::new(Arc::new(memory.clone()), "pbs", "run");
        journals
            .submitted("run-1-1", "17", json!({"scope": "cluster-a"}))
            .unwrap();
        journals.submitted("run-2-1", "17", json!({})).unwrap();
        let other = scheduler("cluster-b", true);
        let found = crate::orphans::stop(&other, Some(&journals), "run")
            .await
            .unwrap();
        assert_eq!(found.len(), 2);
        assert!(
            found
                .iter()
                .all(|orphan| matches!(orphan.outcome, crate::orphans::Outcome::Kept(_)))
        );
        let reported = crate::orphans::report(&other, Some(&journals), "run")
            .await
            .unwrap();
        assert!(
            reported
                .iter()
                .all(|orphan| matches!(orphan.outcome, crate::orphans::Outcome::Unclear(_)))
        );
        assert!(
            lock_calls(&other.calls).is_empty(),
            "no call reaches another scheduler"
        );
        assert_eq!(
            memory.list("pbs", "run").unwrap().len(),
            2,
            "both journals stay"
        );
        let own = scheduler("cluster-a", true);
        crate::orphans::stop(&own, Some(&journals), "run")
            .await
            .unwrap();
        assert_eq!(memory.list("pbs", "run").unwrap(), ["run-2-1"]);
    }

    #[test]
    fn excludes_held_attempts() {
        let memory = MemoryJournal::new();
        let journals = Journals::new(Arc::new(memory.clone()), "pbs", "run");
        let held = journals.hold("run-1-1").unwrap();
        assert!(journals.hold("run-1-1").is_err(), "held by the attempt");
        journals
            .submitted("run-1-1", "17", json!({"queue": "q"}))
            .unwrap();
        let saved = memory.load("pbs", "run-1-1").unwrap().unwrap();
        assert_eq!(
            saved,
            json!({"run": "run", "backend": "pbs", "id": "17", "queue": "q"})
        );
        drop(held);
        journals.ended("run-1-1");
        assert!(memory.list("pbs", "run").unwrap().is_empty());
    }
}
