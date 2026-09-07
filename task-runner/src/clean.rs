//! Clean-up of a store by rule: what no rule keeps goes, and never what a run that goes on may
//! still need. The rules keep results by the time of their last use and by how long their task
//! ran, and keep what named runs refer to. A plan lists what would go before anything goes.
//!
//! What refers to what: a cache entry names its output files (under `runs/`) and the contents
//! of its captured streams and directories (in `cas/` and `trees/`). A run refers to the files
//! of its tasks, which the run database knows. So a file stays when a kept entry or a kept run
//! refers to it; entries go before files, so that no request ever gets a result whose files
//! are gone.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, SystemTime};

use content_store::{FileIdentity, Holder, Lease, Object, Store, layout};
use task_model::{Digest, TreeEntry};

use crate::{CacheEntry, RunnerError};

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
#[derive(Debug)]
pub struct Maintenance {
    _lease: Lease,
    store: String,
}

impl Maintenance {
    /// Expiring leases cannot fence a deletion that is delayed past their expiry.
    pub fn ensure_cleanup(&self) -> Result<(), RunnerError> {
        if self._lease.is_local() {
            return Ok(());
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "destructive cache cleanup requires a non-expiring local lease",
        )
        .into())
    }

    pub async fn check(&self) -> Result<(), RunnerError> {
        Ok(self._lease.check().await?)
    }
}

fn store_name(store: &Store) -> String {
    store
        .local_path("")
        .map_or_else(|| store.url(""), |path| path.to_string_lossy().into_owned())
}

/// Holds the store's maintenance gate until dropped; a concurrent operation reports busy.
pub async fn maintenance(store: &Store) -> Result<Maintenance, RunnerError> {
    let holder = Holder::new("maintenance", "cleanup");
    let lease = store
        .try_lease("maintenance", &holder)
        .await?
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "cache maintenance is in progress",
            )
        })?;
    Ok(Maintenance {
        _lease: lease,
        store: store_name(store),
    })
}

pub(crate) async fn admission(store: &Store) -> Result<Maintenance, RunnerError> {
    loop {
        match maintenance(store).await {
            Err(RunnerError::Io(error)) if error.kind() == std::io::ErrorKind::WouldBlock => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            result => return result,
        }
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

/// The entries of the store by key, with the time each was made and last used.
struct Found {
    namespace: String,
    key: Digest,
    entry: Option<CacheEntry>,
    last_use: Option<SystemTime>,
}

/// What clean-up would remove from a store by the rules of `keep`.
pub async fn plan(store: &Store, keep: &Keep) -> Result<Plan, RunnerError> {
    let original = keep.clone();
    let mut protection = keep.clone();
    let leases = store.held_leases("runs/").await?;
    for holder in &leases {
        protection.runs.insert(holder.run.clone());
        let since = SystemTime::UNIX_EPOCH + Duration::from_millis(holder.since.max(0) as u64);
        protection.active_since = Some(
            protection
                .active_since
                .map_or(since, |earlier| earlier.min(since)),
        );
    }
    let keep = &protection;
    let recent = |time: Option<SystemTime>| {
        let after = |since: Option<SystemTime>| since.is_some_and(|since| time >= Some(since));
        after(keep.used_since) || after(keep.active_since)
    };
    let mut plan = Plan {
        keep: Some(original),
        ..Plan::default()
    };
    // The entries, by the time of their last use: the entry itself, or the mark of its use.
    let mut found: HashMap<(String, Digest), Found> = HashMap::new();
    for object in store.objects("ac/").await? {
        let Some((namespace, key)) = layout::entry_key(&object.location) else {
            continue;
        };
        let item = found
            .entry((namespace.clone(), key.clone()))
            .or_insert_with(|| Found {
                namespace,
                key,
                entry: None,
                last_use: None,
            });
        item.last_use = item.last_use.max(object.modified);
    }
    for item in found.values_mut() {
        if let Some(bytes) = store.get_entry(&item.namespace, &item.key).await? {
            item.entry = CacheEntry::parse(&item.key, &bytes).ok();
        }
    }
    plan.stored_results = found.values().filter(|item| item.entry.is_some()).count();
    // The files that stay: those of kept runs, and those of kept entries.
    let mut locations = keep.locations.clone();
    let mut digests = keep.digests.clone();
    let mut manifests = keep.digests.clone();
    for item in found.values() {
        let kept = match &item.entry {
            Some(entry) => {
                recent(item.last_use)
                    || keep.runs.contains(&entry.run)
                    || keep.slower_than.is_some_and(|slower| {
                        (entry.milliseconds).is_some_and(|took| took >= slower.as_millis() as u64)
                    })
                    || (entry.files.iter()).any(|file| keep.locations.contains(&file.location))
                    || entry.logs().any(|digest| keep.digests.contains(digest))
            }
            // An entry that cannot be read answers no request; it goes unless it is new.
            None => recent(item.last_use),
        };
        if !kept {
            plan.results.push(EntryToRemove {
                namespace: item.namespace.clone(),
                key: item.key.clone(),
                run: item.entry.as_ref().map(|entry| entry.run.clone()),
                task: item.entry.as_ref().map(|entry| entry.task.clone()),
            });
            continue;
        }
        let Some(entry) = &item.entry else { continue };
        for file in &entry.files {
            locations.insert(file.location.clone());
            digests.insert(file.digest.clone());
            if file.directory {
                manifests.insert(file.digest.clone());
            }
        }
        digests.extend(entry.logs().cloned());
    }
    // A file stays when it, or a folder around it, is kept: a directory output or the folder
    // of a kept run.
    let kept_file = |location: &str| {
        let run = location.split('/').nth(1).unwrap_or_default();
        let folders = location.match_indices('/').map(|(end, _)| &location[..end]);
        keep.runs.contains(run)
            || locations.contains(location)
            || folders.into_iter().any(|folder| locations.contains(folder))
    };
    let mut all = Vec::new();
    for kind in ["runs/", "cas/", "trees/"] {
        all.extend(store.objects(kind).await?);
    }
    if leases.is_empty() {
        for object in all
            .iter()
            .filter(|object| object.location.starts_with("trees/"))
        {
            if let Some(digest) = digest_of(&object.location)
                && (digests.contains(&digest) || recent(object.modified))
            {
                manifests.insert(digest);
            }
        }
        for manifest in manifests {
            digests.extend(tree_contents(store, &manifest).await?);
        }
    }
    for object in &all {
        let kept = if object.location.starts_with("runs/") {
            kept_file(&object.location)
        } else {
            // Prepared inputs have no durable per-run digest records yet.
            !leases.is_empty()
                || digest_of(&object.location).is_none_or(|digest| digests.contains(&digest))
        };
        if !kept && !recent(object.modified) {
            plan.objects.push(object.clone());
        }
    }
    plan.stored_objects = all.len();
    plan.stored_bytes = space(&all);
    Ok(plan)
}

/// The digests of the files of a stored directory; none when its manifest is gone.
async fn tree_contents(store: &Store, digest: &Digest) -> Result<Vec<Digest>, RunnerError> {
    if !store.has_tree(digest).await? {
        return Ok(Vec::new());
    }
    let tree = store.get_tree(digest).await?;
    Ok(tree
        .entries
        .into_iter()
        .filter_map(|entry| match entry {
            TreeEntry::File { digest, .. } => Some(digest),
            TreeEntry::EmptyDirectory { .. } | TreeEntry::Symlink { .. } => None,
        })
        .collect())
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

/// Replans with current run protection under the gate before deleting any listed object.
pub async fn remove_guarded(
    store: &Store,
    planned: &Plan,
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
    let keep = planned.keep.as_ref().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "cleanup requires a plan made by clean::plan",
        )
    })?;
    let plan = plan(store, keep).await?;
    let mut removed = Removed::default();
    for result in &plan.results {
        guard.check().await?;
        store.remove_entry(&result.namespace, &result.key).await?;
        guard.check().await?;
        removed.results += 1;
    }
    for object in &plan.objects {
        guard.check().await?;
        store.remove(&object.location).await?;
        guard.check().await?;
        removed.objects += 1;
    }
    guard.check().await?;
    removed.leases = store.remove_free_leases().await?;
    guard.check().await?;
    Ok(removed)
}
