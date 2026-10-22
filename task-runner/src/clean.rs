//! Plans and applies retention rules without deleting content needed by active runs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::time::{Duration, SystemTime};

use content_store::{FileIdentity, Holder, Lease, Object, Store};
use task_model::Digest;

use crate::RunnerError;

#[path = "clean_index.rs"]
mod index;
pub use index::{Report, Summary, scan};

/// What clean-up keeps. Everything else goes.
#[derive(Clone, Debug, Default)]
pub struct Keep {
    /// Results made or used at or after this time.
    pub used_since: Option<SystemTime>,
    /// Results whose task ran at least this long.
    pub slower_than: Option<Duration>,
    /// Runs whose own folder stays with every result they made, like the runs that the user
    /// names and the runs that go on.
    pub runs: BTreeSet<String>,
    /// Files that the kept runs refer to, by their locations in the store, like the results of
    /// earlier runs that they took from the cache.
    pub locations: BTreeSet<String>,
    /// Contents that the kept runs refer to, by digest, like a task script or a log.
    pub digests: BTreeSet<Digest>,
    /// Nothing made or used at or after this time goes, as a run that goes on may need it: the
    /// start of the oldest run that goes on.
    pub active_since: Option<SystemTime>,
}

/// A cache entry that clean-up removes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntryToRemove {
    pub namespace: String,
    pub key: Digest,
    /// The run and the task that made it, when the entry can be read.
    pub run: Option<String>,
    pub task: Option<String>,
}

/// What clean-up removes, and what the store holds in all.
#[derive(Clone, Debug, Default)]
pub struct Plan {
    /// The cache entries that go.
    pub results: Vec<EntryToRemove>,
    /// The files that go: task outputs, contents and directory manifests.
    pub objects: Vec<Object>,
    /// How many cache entries and objects the store holds.
    pub stored_results: usize,
    pub stored_objects: usize,
    /// The space that the objects of the store take, each file once.
    pub stored_bytes: u64,
    keep: Option<Keep>,
}

/// Excludes new input preparation and cache acquisition while cleanup takes fresh snapshots.
/// Clean-up holds the gate alone; runs hold it shared, so that their tasks do not wait for
/// each other.
#[derive(Debug)]
pub struct Maintenance {
    _lease: Option<Lease>,
    store: String,
    alone: bool,
}

impl Maintenance {
    /// Expiring leases cannot fence a deletion that is delayed past their expiry.
    pub fn ensure_cleanup(&self) -> Result<(), RunnerError> {
        if self.alone && self._lease.as_ref().is_some_and(Lease::is_local) {
            return Ok(());
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "destructive cache cleanup requires a non-expiring local lease",
        )
        .into())
    }

    pub async fn check(&self) -> Result<(), RunnerError> {
        match &self._lease {
            Some(lease) => Ok(lease.check().await?),
            None => Ok(()),
        }
    }
}

fn store_name(store: &Store) -> String {
    store
        .local_path("")
        .map_or_else(|| store.url(""), |path| path.to_string_lossy().into_owned())
}

/// The lease that clean-up holds while it waits for the gate: tasks start no new phase
/// meanwhile, so that the ones that go on end and clean-up gets its turn.
const QUEUE: &str = "maintenance-queue";

/// Holds the store's maintenance gate until dropped; a concurrent operation reports busy.
pub async fn maintenance(store: &Store) -> Result<Maintenance, RunnerError> {
    maintenance_within(store, Duration::ZERO, || {}).await
}

/// Holds the gate like [`maintenance`], but waits up to `wait` for tasks that prepare their
/// inputs; `waiting` is called once when the wait begins.
pub async fn maintenance_within(
    store: &Store,
    wait: Duration,
    waiting: impl FnOnce(),
) -> Result<Maintenance, RunnerError> {
    let holder = Holder::new("maintenance", "cleanup");
    let deadline = tokio::time::Instant::now() + wait;
    let mut waiting = Some(waiting);
    let mut queue = None;
    loop {
        // Only leases in a folder are shared, so only there tasks wait in the queue.
        if queue.is_none() && store.local_leases() {
            queue = store.try_lease(QUEUE, &holder).await?;
        }
        if let Some(lease) = store.try_lease("maintenance", &holder).await? {
            return Ok(Maintenance {
                _lease: Some(lease),
                store: store_name(store),
                alone: true,
            });
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "the store is busy: tasks prepare their inputs, or another clean-up goes on",
            )
            .into());
        }
        if let Some(waiting) = waiting.take() {
            waiting();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Holds the gate shared with the other tasks and runs; waits while clean-up holds it or
/// waits for it.
pub(crate) async fn admission(store: &Store) -> Result<Maintenance, RunnerError> {
    let gate = |lease| Maintenance {
        _lease: lease,
        store: store_name(store),
        alone: false,
    };
    // Clean-up removes nothing with leases that expire (see `ensure_cleanup`), so there is
    // nothing to wait for, and object stores cannot share a lease.
    if !store.local_leases() {
        return Ok(gate(None));
    }
    loop {
        if store.try_shared_lease(QUEUE).await?.is_some()
            && let Some(lease) = store.try_shared_lease("maintenance").await?
        {
            return Ok(gate(Some(lease)));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// What clean-up removed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Removed {
    pub results: usize,
    pub objects: usize,
    /// Leases that nobody held any more.
    pub leases: usize,
}

impl Plan {
    /// The space that removing the objects frees. A file of a store on this machine may have
    /// several names, like a task output and its content in `cas/`; it frees its space only
    /// when all of its names go.
    pub fn freed(&self) -> u64 {
        space(&self.objects)
    }

    /// Whether nothing goes.
    pub fn is_empty(&self) -> bool {
        self.results.is_empty() && self.objects.is_empty()
    }
}

impl Report {
    fn collect(self, keep: &Keep) -> Result<Plan, RunnerError> {
        let mut plan = Plan {
            stored_results: self.summary.stored_results,
            stored_objects: self.summary.stored_objects,
            stored_bytes: self.summary.stored_bytes,
            keep: Some(keep.clone()),
            ..Plan::default()
        };
        let mut cursor = 0;
        while let Some((id, entry, _)) = self.next_entry(cursor)? {
            cursor = id;
            if self.sql(|db| {
                db.query_row("SELECT remove FROM entries WHERE id=?1", [id], |row| {
                    row.get::<_, bool>(0)
                })
            })? {
                plan.results.push(entry);
            }
        }
        cursor = 0;
        while let Some((id, object)) = self.next_object(cursor)? {
            cursor = id;
            plan.objects.push(object);
        }
        Ok(plan)
    }
}

/// The space that objects take, each file once; a file of which only some names are listed
/// takes none of it.
fn space(objects: &[Object]) -> u64 {
    let mut names: HashMap<(u64, u64), (u64, u64, u64)> = HashMap::new();
    let mut total = 0;
    for object in objects {
        match object.file {
            Some(FileIdentity {
                device,
                inode,
                names: all,
            }) => {
                let counted = names
                    .entry((device, inode))
                    .or_insert((0, all, object.size));
                counted.0 += 1;
            }
            None => total += object.size,
        }
    }
    let whole = names.values().filter(|(listed, all, _)| listed >= all);
    total + whole.map(|(_, _, size)| size).sum::<u64>()
}

/// Collects the bounded scanner into the legacy in-memory preview representation.
pub async fn plan(store: &Store, keep: &Keep) -> Result<Plan, RunnerError> {
    let scratch = store
        .local_path("cleanup")
        .unwrap_or_else(std::env::temp_dir);
    scan(store, keep, &scratch).await?.collect(keep)
}

/// The digest of a content or a directory manifest, from its location, like
/// `cas/sha256/ab/cdef...`.
fn digest_of(location: &str) -> Option<Digest> {
    let mut parts = location.split('/').skip(1);
    let algorithm = parts.next()?;
    let hex = format!("{}{}", parts.next()?, parts.next()?);
    format!("{algorithm}:{hex}").parse().ok()
}

/// Replans the captured keeping rules under maintenance before removing entries and files.
pub async fn remove(store: &Store, planned: &Plan) -> Result<Removed, RunnerError> {
    let guard = maintenance(store).await?;
    remove_guarded(store, planned, &guard).await
}

/// Evicts only a layered store's local cache after its readers release their local pins.
/// Shared-store garbage collection still requires its own supported deletion authority.
pub async fn evict_front(store: &Store, keep: &Keep) -> Result<Removed, RunnerError> {
    let front = store
        .front()
        .filter(|_| store.shared().is_some())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "front eviction requires a layered local cache",
            )
        })?;
    let guard = maintenance(&front).await?;
    let scratch = front.local_path("cleanup").expect("a local front");
    apply_guarded(&front, keep, &scratch, &guard).await
}

/// Replans with current run protection under the gate before deleting any listed object.
pub async fn remove_guarded(
    store: &Store,
    planned: &Plan,
    guard: &Maintenance,
) -> Result<Removed, RunnerError> {
    let keep = planned.keep.as_ref().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "cleanup requires a plan made by clean::plan",
        )
    })?;
    let scratch = store
        .local_path("cleanup")
        .unwrap_or_else(std::env::temp_dir);
    apply_guarded(store, keep, &scratch, guard).await
}

/// Re-scans under non-expiring authority and consumes the disk-backed deletion queue.
pub async fn apply_guarded(
    store: &Store,
    keep: &Keep,
    scratch: &Path,
    guard: &Maintenance,
) -> Result<Removed, RunnerError> {
    if guard.store != store_name(store) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "maintenance guard belongs to another store",
        )
        .into());
    }
    guard.ensure_cleanup()?;
    guard.check().await?;
    let report = scan(store, keep, scratch).await?;
    let mut removed = Removed::default();
    let mut cursor = 0;
    while let Some((id, entry, _)) = report.next_entry(cursor)? {
        cursor = id;
        if !report.sql(|db| {
            db.query_row("SELECT remove FROM entries WHERE id=?1", [id], |row| {
                row.get::<_, bool>(0)
            })
        })? {
            continue;
        }
        guard.check().await?;
        let deleted = store
            .remove_entry_observed(&entry.namespace, &entry.key, &report.entry_objects(id)?)
            .await?;
        guard.check().await?;
        removed.results += usize::from(deleted);
    }
    cursor = 0;
    while let Some((id, object)) = report.next_object(cursor)? {
        cursor = id;
        guard.check().await?;
        let deleted = store.remove_observed(&object).await?;
        guard.check().await?;
        removed.objects += usize::from(deleted);
    }
    guard.check().await?;
    removed.leases = store.remove_free_leases().await?;
    guard.check().await?;
    Ok(removed)
}
