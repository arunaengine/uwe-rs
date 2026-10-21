//! Caches local file digests only while their observed identity stays unchanged.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use content_store::StoreError;
use rusqlite::{Connection, OptionalExtension, params};
use task_model::Digest;

/// A file whose content changed this shortly before it was read is not kept: on a file system
/// whose times are coarse, a change right after the read could keep the same time.
const RECENT: Duration = Duration::from_secs(2);

/// A smaller file is read rather than looked up: reading it takes less time than a query of
/// the cache, above all on a file system that cluster nodes share, where each query and each
/// commit waits for a lock.
const SMALLEST: u64 = 1 << 20;

/// What tells one version of a file from another. Times are in nanoseconds since 1970.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Version {
    device: u64,
    inode: u64,
    size: u64,
    modified: i64,
    changed: i64,
}

impl Version {
    fn of(metadata: &std::fs::Metadata) -> Version {
        let nanoseconds = |seconds: i64, nanoseconds: i64| {
            seconds
                .saturating_mul(1_000_000_000)
                .saturating_add(nanoseconds)
        };
        Version {
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.size(),
            modified: nanoseconds(metadata.mtime(), metadata.mtime_nsec()),
            changed: nanoseconds(metadata.ctime(), metadata.ctime_nsec()),
        }
    }

    /// Whether the file changed less than `window` before `now`, so that a later change could
    /// keep the same times.
    fn recent(&self, now: SystemTime, window: Duration) -> bool {
        let now = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos() as i64);
        now.saturating_sub(self.modified.max(self.changed)) < window.as_nanos() as i64
    }
}

/// The cache of the digests of local files. Its clones share one connection.
#[derive(Clone)]
pub struct DigestCache {
    connection: Arc<Mutex<Connection>>,
    /// A file that changed less than this before it was read is not kept.
    recent: Duration,
    /// A file smaller than this is read, not looked up or kept.
    smallest: u64,
}

impl fmt::Debug for DigestCache {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DigestCache")
    }
}

impl DigestCache {
    /// The name of the file of the cache in the cache folder of uwe.
    pub const FILE_NAME: &str = "digests.sqlite";

    /// Opens the cache in the file `path`, making it and its folder when they are missing.
    pub fn open(path: &Path) -> Result<DigestCache, rusqlite::Error> {
        if let Some(folder) = path.parent() {
            let _ = std::fs::create_dir_all(folder);
        }
        let connection = Connection::open(path)?;
        // WAL, or the rollback journal on a file system that machines share.
        let journal = run_database::journal(path);
        connection.busy_timeout(journal.wait)?;
        connection.pragma_update(None, "journal_mode", journal.mode)?;
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS digests (
                path TEXT PRIMARY KEY,
                device INTEGER NOT NULL,
                inode INTEGER NOT NULL,
                size INTEGER NOT NULL,
                modified INTEGER NOT NULL,
                changed INTEGER NOT NULL,
                digest TEXT NOT NULL
            )",
        )?;
        Ok(DigestCache {
            connection: Arc::new(Mutex::new(connection)),
            recent: RECENT,
            smallest: SMALLEST,
        })
    }

    /// The digest and size of a local file: from the cache when the file has not changed since
    /// it was read, else read now and kept for the next time.
    pub async fn hash(&self, file: &Path) -> Result<(Digest, u64), StoreError> {
        let metadata = tokio::fs::metadata(file).await.map_err(at(file))?;
        let version = Version::of(&metadata);
        if version.size < self.smallest {
            return content_store::hash_file(file).await;
        }
        let path = file.to_string_lossy().into_owned();
        if let Some(digest) = self.known(path.clone(), version).await {
            return Ok((digest, version.size));
        }
        let (digest, size) = content_store::hash_file(file).await?;
        let after = tokio::fs::metadata(file)
            .await
            .map(|after| Version::of(&after));
        // A file that changed while it was read, or that may change again unnoticed, is not
        // kept.
        let unchanged = after.is_ok_and(|after| after == version) && size == version.size;
        if unchanged && !version.recent(SystemTime::now(), self.recent) {
            self.keep(path, version, digest.clone()).await;
        }
        Ok((digest, size))
    }

    /// Keeps the digest of a file that the caller has just read itself and that nothing
    /// changes, like an output that the run moved into its store, even when the file is
    /// recent: a later task that reads it as its input need not read it again.
    pub async fn keep_stored(&self, file: &Path, digest: &Digest) {
        let Ok(metadata) = tokio::fs::metadata(file).await else {
            return;
        };
        let path = file.to_string_lossy().into_owned();
        self.keep(path, Version::of(&metadata), digest.clone())
            .await;
    }

    /// The digest of the path in the cache, when it was kept for this version of the file.
    async fn known(&self, path: String, version: Version) -> Option<Digest> {
        let connection = self.connection.clone();
        let found = tokio::task::spawn_blocking(move || {
            let connection = connection.lock().ok()?;
            let row = connection
                .query_row(
                    "SELECT device, inode, size, modified, changed, digest FROM digests \
                     WHERE path = ?1",
                    [&path],
                    |row| {
                        let version = Version {
                            device: row.get::<_, i64>(0)? as u64,
                            inode: row.get::<_, i64>(1)? as u64,
                            size: row.get::<_, i64>(2)? as u64,
                            modified: row.get(3)?,
                            changed: row.get(4)?,
                        };
                        Ok((version, row.get::<_, String>(5)?))
                    },
                )
                .optional()
                .ok()??;
            Some(row)
        });
        let (kept, digest) = found.await.ok()??;
        (kept == version).then(|| digest.parse().ok())?
    }

    /// Keeps the digest of a version of the file at `path`, in place of an older one.
    async fn keep(&self, path: String, version: Version, digest: Digest) {
        let connection = self.connection.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let Ok(connection) = connection.lock() else {
                return;
            };
            let _ = connection.execute(
                "INSERT OR REPLACE INTO digests (path, device, inode, size, modified, changed, \
                 digest) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    path,
                    version.device as i64,
                    version.inode as i64,
                    version.size as i64,
                    version.modified,
                    version.changed,
                    digest.to_string()
                ],
            );
        })
        .await;
    }
}

/// An error of a file of this machine, with its path.
fn at(path: &Path) -> impl FnOnce(std::io::Error) -> StoreError + '_ {
    move |error| StoreError::File {
        path: path.to_owned(),
        error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("a runtime")
            .block_on(future)
    }

    /// A cache that keeps files however shortly before they changed, as the tests change them
    /// at once.
    fn open(path: &Path) -> DigestCache {
        let mut cache = DigestCache::open(path).expect("opens the cache");
        cache.recent = Duration::ZERO;
        cache.smallest = 0;
        cache
    }

    #[test]
    fn caches_unchanged_digests() {
        let folder = tempfile::tempdir().expect("a temporary directory");
        let cache = open(&folder.path().join("cache/digests.sqlite"));
        let file = folder.path().join("reads.fq");
        std::fs::write(&file, "ACGT\n").expect("writes");
        let (digest, size) = block_on(cache.hash(&file)).expect("hashes");
        assert_eq!(digest, Digest::sha256(b"ACGT\n"));
        assert_eq!(size, 5);
        let version = Version::of(&std::fs::metadata(&file).expect("reads the metadata"));
        let path = file.to_string_lossy().into_owned();
        assert_eq!(
            block_on(cache.known(path.clone(), version)),
            Some(digest.clone())
        );
        // The same size, but another change: read again.
        std::fs::write(&file, "TTTT\n").expect("writes");
        let (changed, _) = block_on(cache.hash(&file)).expect("hashes");
        assert_eq!(changed, Digest::sha256(b"TTTT\n"));
        // Another process sees the same cache.
        let other = open(&folder.path().join("cache/digests.sqlite"));
        let version = Version::of(&std::fs::metadata(&file).expect("reads the metadata"));
        assert_eq!(block_on(other.known(path, version)), Some(changed));
    }

    #[test]
    fn avoids_changed_files() {
        let folder = tempfile::tempdir().expect("a temporary directory");
        let mut cache = DigestCache::open(&folder.path().join("digests.sqlite")).expect("opens");
        cache.smallest = 0;
        let file = folder.path().join("new.txt");
        std::fs::write(&file, "new\n").expect("writes");
        block_on(cache.hash(&file)).expect("hashes");
        let version = Version::of(&std::fs::metadata(&file).expect("reads the metadata"));
        let path = file.to_string_lossy().into_owned();
        assert_eq!(block_on(cache.known(path, version)), None);
    }

    #[test]
    fn recognizes_stored_files() {
        let folder = tempfile::tempdir().expect("a temporary directory");
        let cache = open(&folder.path().join("digests.sqlite"));
        let file = folder.path().join("output.bam");
        std::fs::write(&file, "made\n").expect("writes");
        // The digest comes from the cache, not from reading the file.
        let told = Digest::sha256(b"told");
        block_on(cache.keep_stored(&file, &told));
        assert_eq!(block_on(cache.hash(&file)).expect("hashes"), (told, 5));
    }

    #[test]
    fn reads_small_files() {
        let folder = tempfile::tempdir().expect("a temporary directory");
        let mut cache = DigestCache::open(&folder.path().join("digests.sqlite")).expect("opens");
        cache.recent = Duration::ZERO;
        let file = folder.path().join("small.txt");
        std::fs::write(&file, "small\n").expect("writes");
        let (digest, size) = block_on(cache.hash(&file)).expect("hashes");
        assert_eq!((digest, size), (Digest::sha256(b"small\n"), 6));
        let version = Version::of(&std::fs::metadata(&file).expect("reads the metadata"));
        let path = file.to_string_lossy().into_owned();
        assert_eq!(block_on(cache.known(path, version)), None);
    }
}
