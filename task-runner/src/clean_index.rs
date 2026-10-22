//! Spools cleanup inventory and retention marks into a bounded temporary SQLite index.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use content_store::{Object, Store, StoredDirectory, layout};
use rusqlite::{Connection, OptionalExtension, params};
use task_model::{Digest, TreeEntry};

use super::{EntryToRemove, Keep, digest_of};
use crate::{CacheEntry, RunnerError};

/// Counts and exact hard-link accounting, independent of store object count.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub stored_results: usize,
    pub stored_objects: usize,
    pub stored_bytes: u64,
    pub results: usize,
    pub objects: usize,
    pub freed: u64,
    pub contents: usize,
}

/// A temporary disk-backed preview; removal always obtains a fresh scan under maintenance.
pub struct Report {
    pub summary: Summary,
    connection: Mutex<Connection>,
    _directory: tempfile::TempDir,
}

impl Report {
    fn open(scratch: &Path) -> Result<Self, RunnerError> {
        std::fs::create_dir_all(scratch)?;
        let directory = tempfile::tempdir_in(scratch)?;
        let connection =
            Connection::open(directory.path().join("cleanup.sqlite")).map_err(failed)?;
        connection
            .execute_batch(
                "PRAGMA cache_size=-2048; PRAGMA temp_store=FILE; PRAGMA mmap_size=0; PRAGMA journal_mode=OFF;
             PRAGMA synchronous=OFF; BEGIN;
             CREATE TABLE entries(id INTEGER PRIMARY KEY, namespace TEXT, key TEXT, recent INTEGER,
                 remove INTEGER DEFAULT 0, run TEXT, task TEXT, body BLOB, used BLOB, UNIQUE(namespace,key));
             CREATE TABLE marks(kind TEXT, key TEXT, PRIMARY KEY(kind,key)) WITHOUT ROWID;
             CREATE TABLE objects(id INTEGER PRIMARY KEY, body BLOB, remove INTEGER, run TEXT);
             CREATE TABLE runs(run TEXT PRIMARY KEY, results INTEGER, objects INTEGER) WITHOUT ROWID;
             CREATE TABLE inodes(identity TEXT PRIMARY KEY, names INTEGER, listed INTEGER,
                 removed INTEGER, size INTEGER) WITHOUT ROWID;",
            )
            .map_err(failed)?;
        Ok(Self {
            summary: Summary::default(),
            connection: Mutex::new(connection),
            _directory: directory,
        })
    }

    pub(super) fn sql<T>(
        &self,
        action: impl FnOnce(&Connection) -> rusqlite::Result<T>,
    ) -> Result<T, RunnerError> {
        action(
            &self
                .connection
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()),
        )
        .map_err(failed)
    }

    fn mark(&self, kind: &str, key: &str) -> Result<(), RunnerError> {
        self.sql(|db| {
            db.execute(
                "INSERT OR IGNORE INTO marks VALUES(?1,?2)",
                params![kind, key],
            )
        })?;
        Ok(())
    }

    fn contains(&self, kind: &str, key: &str) -> Result<bool, RunnerError> {
        self.sql(|db| {
            db.query_row(
                "SELECT EXISTS(SELECT 1 FROM marks WHERE kind=?1 AND key=?2)",
                params![kind, key],
                |row| row.get(0),
            )
        })
    }

    fn count_run(&self, run: &str, results: usize, objects: usize) -> Result<(), RunnerError> {
        self.sql(|db| {
            db.execute(
                "INSERT INTO runs VALUES(?1,?2,?3) ON CONFLICT(run) DO UPDATE
                 SET results=results+excluded.results,objects=objects+excluded.objects",
                params![run, results, objects],
            )
        })?;
        Ok(())
    }

    fn related(&self, location: &str, descendants: bool) -> Result<bool, RunnerError> {
        if self.contains("location", location)? {
            return Ok(true);
        }
        for (end, _) in location.match_indices('/') {
            if self.contains("location", &location[..end])? {
                return Ok(true);
            }
        }
        if !descendants {
            return Ok(false);
        }
        self.sql(|db| {
            db.query_row(
                "SELECT EXISTS(SELECT 1 FROM marks WHERE kind='location' AND key>=?1 AND key<?2)",
                params![format!("{location}/"), format!("{location}0")],
                |row| row.get(0),
            )
        })
    }

    pub(super) fn next_entry(
        &self,
        cursor: i64,
    ) -> Result<Option<(i64, EntryToRemove, bool)>, RunnerError> {
        let next = self.sql(|db| db.query_row(
            "SELECT id,namespace,key,recent,run,task FROM entries WHERE id>?1 ORDER BY id LIMIT 1",
            [cursor], |row| Ok((row.get::<_,i64>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,
                row.get::<_,bool>(3)?,row.get::<_,Option<String>>(4)?,row.get::<_,Option<String>>(5)?)))
            .optional())?;
        next.map(|(id, namespace, key, recent, run, task)| {
            Ok((
                id,
                EntryToRemove {
                    namespace,
                    key: key.parse().map_err(std::io::Error::other)?,
                    run,
                    task,
                },
                recent,
            ))
        })
        .transpose()
    }

    pub(super) fn next_object(&self, cursor: i64) -> Result<Option<(i64, Object)>, RunnerError> {
        let next = self.sql(|db| {
            db.query_row(
                "SELECT id,body FROM objects WHERE remove=1 AND id>?1 ORDER BY id LIMIT 1",
                [cursor],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
            )
            .optional()
        })?;
        next.map(|(id, bytes)| {
            Ok((
                id,
                serde_json::from_slice(&bytes).map_err(std::io::Error::other)?,
            ))
        })
        .transpose()
    }

    pub(super) fn entry_objects(&self, id: i64) -> Result<Vec<Object>, RunnerError> {
        let (body, used): (Option<Vec<u8>>, Option<Vec<u8>>) = self.sql(|db| {
            db.query_row("SELECT body,used FROM entries WHERE id=?1", [id], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
        })?;
        [body, used]
            .into_iter()
            .flatten()
            .map(|bytes| {
                serde_json::from_slice(&bytes).map_err(|error| std::io::Error::other(error).into())
            })
            .collect()
    }

    /// Visits removal groups without collecting all runs in memory.
    pub fn visit_runs(&self, mut visit: impl FnMut(&str, usize, usize)) -> Result<(), RunnerError> {
        self.sql(|db| {
            let mut statement = db.prepare("SELECT run,results,objects FROM runs ORDER BY run")?;
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                visit(&row.get::<_, String>(0)?, row.get(1)?, row.get(2)?);
            }
            Ok(())
        })
    }

    pub fn is_empty(&self) -> bool {
        self.summary.results == 0 && self.summary.objects == 0
    }

    pub fn freed(&self) -> u64 {
        self.summary.freed
    }
}

/// Uses disk for inventory, retention marks and inode counts; memory holds one manifest at a time.
pub async fn scan(store: &Store, keep: &Keep, scratch: &Path) -> Result<Report, RunnerError> {
    let store = store.read_only();
    let mut report = Report::open(scratch)?;
    let mut active_since = keep.active_since;
    let mut leases = store.lease_inventory("runs/").await?;
    let mut readers = false;
    for run in &keep.runs {
        report.mark("run", run)?;
    }
    while let Some(holder) = leases.next().await? {
        readers = true;
        report.mark("run", &holder.run)?;
        let since = SystemTime::UNIX_EPOCH + Duration::from_millis(holder.since.max(0) as u64);
        active_since = Some(active_since.map_or(since, |earlier| earlier.min(since)));
    }
    let recent = |time: Option<SystemTime>| {
        [keep.used_since, active_since]
            .into_iter()
            .flatten()
            .any(|since| time >= Some(since))
    };
    for location in &keep.locations {
        report.mark("location", location)?;
    }
    for digest in &keep.digests {
        report.mark("digest", &digest.to_string())?;
        report.mark("manifest", &digest.to_string())?;
    }
    let mut inventory = store.inventory("ac/").await?;
    while let Some(object) = inventory.next().await? {
        if let Some((namespace, key)) = layout::entry_key(&object.location) {
            let bytes = serde_json::to_vec(&object).map_err(std::io::Error::other)?;
            report.sql(|db| {
                db.execute(
                    "INSERT INTO entries(namespace,key,recent,body,used) VALUES(?1,?2,?3,?4,?5)
                 ON CONFLICT(namespace,key) DO UPDATE SET recent=max(recent,excluded.recent),
                     body=coalesce(excluded.body,body),used=coalesce(excluded.used,used)",
                    params![
                        namespace,
                        key.to_string(),
                        recent(object.modified),
                        object.location.ends_with(".json").then_some(&bytes),
                        object.location.ends_with(".used").then_some(&bytes)
                    ],
                )
            })?;
        }
    }
    let mut cursor = 0;
    while let Some((id, item, used)) = report.next_entry(cursor)? {
        cursor = id;
        let entry = store
            .get_entry(&item.namespace, &item.key)
            .await?
            .and_then(|bytes| CacheEntry::parse(&item.key, &bytes).ok());
        report.summary.stored_results += usize::from(entry.is_some());
        let kept = match &entry {
            Some(entry) => {
                used || report.contains("run", &entry.run)?
                    || keep.slower_than.is_some_and(|slower| {
                        entry
                            .milliseconds
                            .is_some_and(|took| took >= slower.as_millis() as u64)
                    })
                    || entry
                        .files
                        .iter()
                        .any(|file| keep.locations.contains(&file.location))
                    || entry.logs().any(|digest| keep.digests.contains(digest))
            }
            None => used,
        };
        report.sql(|db| {
            db.execute(
                "UPDATE entries SET remove=?1,run=?2,task=?3 WHERE id=?4",
                params![
                    !kept,
                    entry.as_ref().map(|entry| &entry.run),
                    entry.as_ref().map(|entry| &entry.task),
                    id
                ],
            )
        })?;
        if !kept {
            report.summary.results += 1;
            report.count_run(entry.as_ref().map_or("", |entry| &entry.run), 1, 0)?;
            continue;
        }
        if let Some(entry) = entry {
            for file in &entry.files {
                report.mark("location", &file.location)?;
                report.mark("digest", &file.digest.to_string())?;
                if file.directory {
                    report.mark("manifest", &file.digest.to_string())?;
                }
            }
            for digest in entry.logs() {
                report.mark("digest", &digest.to_string())?;
            }
        }
    }
    let mut inventory = store.inventory("directories/v1/").await?;
    while let Some(object) = inventory.next().await? {
        if object.folder {
            continue;
        }
        let bytes = store.get_object(&object.location).await?;
        let directory: StoredDirectory =
            serde_json::from_slice(&bytes).map_err(std::io::Error::other)?;
        if layout::directory(&directory.location) != object.location {
            return Err(
                std::io::Error::other("directory descriptor names another location").into(),
            );
        }
        let run = directory
            .location
            .strip_prefix("runs/")
            .and_then(|path| path.split('/').next());
        if readers
            || recent(object.modified)
            || report.contains("digest", &directory.digest.to_string())?
            || run
                .map(|run| report.contains("run", run))
                .transpose()?
                .unwrap_or(false)
            || report.related(&directory.location, true)?
        {
            report.mark("descriptor", &object.location)?;
            report.mark("digest", &directory.digest.to_string())?;
            report.mark("manifest", &directory.digest.to_string())?;
            report.mark("location", &directory.location)?;
        }
    }
    if !readers {
        let mut inventory = store.inventory("trees/").await?;
        while let Some(object) = inventory.next().await? {
            if let Some(digest) = digest_of(&object.location)
                && (report.contains("digest", &digest.to_string())? || recent(object.modified))
            {
                report.mark("manifest", &digest.to_string())?;
            }
        }
        let mut cursor = String::new();
        loop {
            let next = report.sql(|db| {
                db.query_row(
                    "SELECT key FROM marks WHERE kind='manifest' AND key>?1 ORDER BY key LIMIT 1",
                    [&cursor],
                    |row| row.get::<_, String>(0),
                )
                .optional()
            })?;
            let Some(key) = next else { break };
            cursor = key.clone();
            let digest: Digest = key.parse().map_err(std::io::Error::other)?;
            if store.has_tree(&digest).await? {
                for entry in store.get_tree(&digest).await?.entries {
                    if let TreeEntry::File { digest, .. } = entry {
                        report.mark("digest", &digest.to_string())?;
                    }
                }
            }
        }
    }
    for kind in [
        "directories/v1/",
        "runs/",
        "cas/",
        "trees/",
        "directory-snapshots/",
    ] {
        let mut inventory = store.inventory(kind).await?;
        while let Some(object) = inventory.next().await? {
            if kind == "directories/v1/" && object.folder {
                continue;
            }
            let kept = if kind == "directories/v1/" {
                report.contains("descriptor", &object.location)?
            } else if let Some(snapshot) = object.location.strip_prefix("directory-snapshots/v1/") {
                readers
                    || snapshot
                        .split('/')
                        .next()
                        .and_then(|hex| format!("sha256:{hex}").parse::<Digest>().ok())
                        .map(|digest| report.contains("digest", &digest.to_string()))
                        .transpose()?
                        .unwrap_or(false)
            } else if object.location.starts_with("directory-snapshots/legacy/") {
                readers
            } else if kind == "runs/" {
                report.contains("run", object.location.split('/').nth(1).unwrap_or_default())?
                    || report.related(&object.location, false)?
            } else {
                readers
                    || digest_of(&object.location)
                        .map(|digest| report.contains("digest", &digest.to_string()))
                        .transpose()?
                        .unwrap_or(true)
            };
            let remove = !kept && !recent(object.modified);
            let run = object
                .location
                .strip_prefix("runs/")
                .map(|rest| rest.split('/').next().unwrap_or_default());
            let bytes = serde_json::to_vec(&object).map_err(std::io::Error::other)?;
            report.sql(|db| {
                db.execute(
                    "INSERT INTO objects(body,remove,run) VALUES(?1,?2,?3)",
                    params![bytes, remove, run],
                )
            })?;
            report.summary.stored_objects += 1;
            report.summary.objects += usize::from(remove);
            report.summary.contents += usize::from(remove && run.is_none());
            if remove && let Some(run) = run {
                report.count_run(run, 0, 1)?;
            }
            if let Some(file) = object.file {
                let size = i64::try_from(object.size).map_err(std::io::Error::other)?;
                let names = i64::try_from(file.names).map_err(std::io::Error::other)?;
                report.sql(|db| {
                    db.execute(
                        "INSERT INTO inodes VALUES(?1,?2,1,?3,?4) ON CONFLICT(identity)
                     DO UPDATE SET listed=listed+1,removed=removed+excluded.removed",
                        params![
                            format!("{}/{}", file.device, file.inode),
                            names,
                            usize::from(remove),
                            size
                        ],
                    )
                })?;
            } else {
                report.summary.stored_bytes = report
                    .summary
                    .stored_bytes
                    .checked_add(object.size)
                    .ok_or_else(|| std::io::Error::other("store byte count overflow"))?;
                if remove {
                    report.summary.freed = report
                        .summary
                        .freed
                        .checked_add(object.size)
                        .ok_or_else(|| std::io::Error::other("cleanup byte count overflow"))?;
                }
            }
        }
    }
    let (stored, freed): (u64, u64) = report.sql(|db| {
        db.query_row(
            "SELECT coalesce(sum(CASE WHEN listed>=names THEN size ELSE 0 END),0),
                coalesce(sum(CASE WHEN removed>=names THEN size ELSE 0 END),0) FROM inodes",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
    })?;
    report.summary.stored_bytes = report
        .summary
        .stored_bytes
        .checked_add(stored)
        .ok_or_else(|| std::io::Error::other("store byte count overflow"))?;
    report.summary.freed = report
        .summary
        .freed
        .checked_add(freed)
        .ok_or_else(|| std::io::Error::other("cleanup byte count overflow"))?;
    report.sql(|db| db.execute_batch("COMMIT"))?;
    Ok(report)
}

fn failed(error: rusqlite::Error) -> RunnerError {
    std::io::Error::other(error).into()
}
