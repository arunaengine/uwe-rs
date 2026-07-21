//! Leases in the store: one holder at a time for a name, like the cache key of a task that runs
//! now. So two runs that ask for the same task at the same moment run it once: the one that
//! holds the lease runs it, the other waits and then takes the result from the cache.
//!
//! A lease never outlives the process that holds it. In a store on this machine it is a lock on
//! a file under `locks/`, which the kernel frees when the process ends, also when it crashes. In
//! an object store it is an object under `locks/` with an expiry, which the holder renews while
//! it lives, so that the lease of a crashed run ends after [`LEASE_TIME`].
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fs::{File, TryLockError};
use std::io::{ErrorKind as IoErrorKind, Read, Seek, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use opendal::{ErrorKind, Operator};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use crate::{Store, StoreError, layout};

/// How long a lease in an object store lasts when its holder does not renew it.
pub const LEASE_TIME: Duration = Duration::from_secs(60);

/// How often the holder of a lease in an object store renews it.
const RENEWAL: Duration = Duration::from_secs(20);

/// Numbers the leases of this process, so that each has a token of its own.
static NEXT: AtomicU64 = AtomicU64::new(0);

/// Who holds a lease.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Holder {
    /// The run, like `1a0f1cc4c10-3a2487`.
    pub run: String,
    /// What the run holds the lease for, like the name of a task.
    pub purpose: String,
    /// The machine and the process of the run.
    pub host: String,
    pub process: u32,
    /// When the run took the lease, in milliseconds since the Unix epoch.
    pub since: i64,
}

impl Holder {
    /// This process, for the run `run`, from now on.
    pub fn new(run: &str, purpose: &str) -> Holder {
        Holder {
            run: run.to_owned(),
            purpose: purpose.to_owned(),
            host: host_name(),
            process: std::process::id(),
            since: milliseconds_now(),
        }
    }
}

/// A lease that this process holds, until it is dropped.
#[derive(Debug)]
pub struct Lease {
    _hold: Hold,
}

#[derive(Debug)]
enum Hold {
    /// The locked file; closing it frees the lock.
    File { _locked: File },
    /// Dropping it stops the renewal, which then removes the object.
    Object { _renewal: oneshot::Sender<()> },
}

/// A lease in an object store.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Record {
    holder: Holder,
    /// A value that only this lease has, so that a holder never removes a lease that another
    /// took over after its own ended.
    token: String,
    /// When the lease ends unless the holder renews it, in milliseconds since the Unix epoch.
    expires: i64,
}

impl Store {
    /// Takes the lease `name`, like `tasks/<namespace>/<key>`, unless another holds it now.
    pub async fn try_lease(
        &self,
        name: &str,
        holder: &Holder,
    ) -> Result<Option<Lease>, StoreError> {
        let location = layout::lease(name)?;
        match self.local_path(&location) {
            Some(path) => {
                let holder = holder.clone();
                let file = blocking(move || lock_file(&path, &holder)).await?;
                Ok(file.map(|file| Lease {
                    _hold: Hold::File { _locked: file },
                }))
            }
            None => take_object(&self.operator, &location, holder).await,
        }
    }

    /// Who holds the leases whose names start with `prefix`, like `runs/`, now.
    pub async fn held_leases(&self, prefix: &str) -> Result<Vec<Holder>, StoreError> {
        let folder = layout::lease(prefix.trim_end_matches('/'))?;
        let mut held = Vec::new();
        for object in self.objects(&format!("{folder}/")).await? {
            let holder = match self.local_path(&object.location) {
                Some(path) => blocking(move || file_holder(&path)).await?,
                None => read_record(&self.operator, &object.location)
                    .await?
                    .map(|(record, _)| record)
                    .filter(|record| record.expires > milliseconds_now())
                    .map(|record| record.holder),
            };
            held.extend(holder);
        }
        Ok(held)
    }

    /// Removes the leases that nobody holds, and gives how many there were.
    pub async fn remove_free_leases(&self) -> Result<usize, StoreError> {
        let mut removed = 0;
        for object in self.objects(&format!("{}/", layout::LEASES)).await? {
            let free = match self.local_path(&object.location) {
                Some(path) => blocking(move || remove_free_file(&path)).await?,
                None => {
                    let record = read_record(&self.operator, &object.location).await?;
                    let ended =
                        record.is_none_or(|(record, _)| record.expires <= milliseconds_now());
                    if ended {
                        self.operator.delete(&object.location).await?;
                    }
                    ended
                }
            };
            removed += usize::from(free);
        }
        Ok(removed)
    }
}

/// Locks the file at `path` for `holder` and writes who holds it into it; `None` when another
/// holds the lock.
fn lock_file(path: &Path, holder: &Holder) -> std::io::Result<Option<File>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Clean-up may remove a free lock file between the moment it is opened and the moment it
    // is locked. A lock on a file that is no longer at the path counts for nothing, so the
    // lock is taken again on the file that is there now.
    for _ in 0..8 {
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Ok(None),
            Err(TryLockError::Error(error)) => return Err(error),
        }
        if !same_file(&file, path)? {
            continue;
        }
        file.set_len(0)?;
        file.rewind()?;
        file.write_all(&serde_json::to_vec(holder)?)?;
        return Ok(Some(file));
    }
    Err(std::io::Error::other(format!(
        "{}: the lock file keeps being replaced",
        path.display()
    )))
}

/// Whether the open file is the one at `path`.
fn same_file(file: &File, path: &Path) -> std::io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let open = file.metadata()?;
    match std::fs::metadata(path) {
        Ok(named) => Ok(named.dev() == open.dev() && named.ino() == open.ino()),
        Err(error) if error.kind() == IoErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Who holds the lock of the file at `path`, when someone does.
fn file_holder(path: &Path) -> std::io::Result<Option<Holder>> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == IoErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    match file.try_lock_shared() {
        // Nobody holds it; closing the file frees this lock again.
        Ok(()) => return Ok(None),
        Err(TryLockError::WouldBlock) => {}
        Err(TryLockError::Error(error)) => return Err(error),
    }
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    // A holder that is still writing its name is a holder all the same.
    Ok(Some(serde_json::from_str(&text).unwrap_or_default()))
}

/// Removes the lock file at `path` unless someone holds it; whether it was free.
fn remove_free_file(path: &Path) -> std::io::Result<bool> {
    let file = match std::fs::OpenOptions::new().write(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == IoErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Ok(false),
        Err(TryLockError::Error(error)) => return Err(error),
    }
    // Removed while locked: whoever opened it meanwhile finds that it is gone once it has the
    // lock, and takes a new file (see `lock_file`).
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == IoErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Takes the lease object at `location`: a new one, or one whose holder stopped renewing it.
async fn take_object(
    operator: &Operator,
    location: &str,
    holder: &Holder,
) -> Result<Option<Lease>, StoreError> {
    let record = Record {
        holder: holder.clone(),
        token: token(),
        expires: milliseconds_now() + LEASE_TIME.as_millis() as i64,
    };
    let bytes = serde_json::to_vec(&record).map_err(std::io::Error::other)?;
    let created = (operator.write_with(location, bytes.clone()))
        .if_not_exists(true)
        .await;
    match created {
        Ok(_) => return Ok(Some(renewed(operator.clone(), location, record))),
        Err(error) if error.kind() == ErrorKind::ConditionNotMatch => {}
        Err(error) => return Err(error.into()),
    }
    // Someone holds it, or held it and stopped: a crashed run renews nothing.
    let Some((found, tag)) = read_record(operator, location).await? else {
        // Given back just now; the next try takes it.
        return Ok(None);
    };
    if found.expires > milliseconds_now() {
        return Ok(None);
    }
    let conditional = operator.info().capability().write_with_if_match;
    let taken = match tag {
        // Replaced only if it is still the lease that ended, so of two runs that take it over
        // at once, one wins.
        Some(tag) if conditional => operator.write_with(location, bytes).if_match(&tag).await,
        // Without conditional writes, two runs that remove it at the same moment may both take
        // it. The task then runs twice, which costs time but is safe: the first result that
        // is stored wins.
        _ => {
            operator.delete(location).await?;
            (operator.write_with(location, bytes))
                .if_not_exists(true)
                .await
        }
    };
    match taken {
        Ok(_) => Ok(Some(renewed(operator.clone(), location, record))),
        Err(error) if error.kind() == ErrorKind::ConditionNotMatch => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// A lease whose object is renewed until the lease is dropped, and then removed.
fn renewed(operator: Operator, location: &str, mut record: Record) -> Lease {
    let (stop, mut stopped) = oneshot::channel::<()>();
    let location = location.to_owned();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut stopped => break,
                () = tokio::time::sleep(RENEWAL) => {}
            }
            // A holder that was held up for longer than the lease lost it; it must not take it
            // back from the run that took it over.
            match read_record(&operator, &location).await {
                Ok(Some((found, _))) if found.token == record.token => {}
                _ => return,
            }
            record.expires = milliseconds_now() + LEASE_TIME.as_millis() as i64;
            if let Ok(bytes) = serde_json::to_vec(&record) {
                // A failed renewal is tried again at the next turn; the lease lasts until then.
                let _ = operator.write(&location, bytes).await;
            }
        }
        if let Ok(Some((found, _))) = read_record(&operator, &location).await
            && found.token == record.token
        {
            let _ = operator.delete(&location).await;
        }
    });
    Lease {
        _hold: Hold::Object { _renewal: stop },
    }
}

/// The lease object at `location` with its entity tag; `None` when there is none. An object
/// that cannot be read as a lease counts as one that has ended.
async fn read_record(
    operator: &Operator,
    location: &str,
) -> Result<Option<(Record, Option<String>)>, StoreError> {
    let tag = match operator.stat(location).await {
        Ok(metadata) => metadata.etag().map(str::to_owned),
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let bytes = match operator.read(location).await {
        Ok(buffer) => buffer.to_bytes(),
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let record = serde_json::from_slice(&bytes).unwrap_or_else(|_| Record {
        holder: Holder::default(),
        token: String::new(),
        expires: 0,
    });
    Ok(Some((record, tag)))
}

/// Runs file operations that may block on a thread for them.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> std::io::Result<T> + Send + 'static,
) -> Result<T, StoreError> {
    let result = tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    Ok(result?)
}

/// A value that no other lease has.
fn token() -> String {
    let number = NEXT.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    format!("{}-{}-{nanos:x}-{number}", host_name(), std::process::id())
}

/// The name of this machine, as Linux gives it.
fn host_name() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|name| name.trim().to_owned())
        .unwrap_or_else(|_| "unknown".to_owned())
}

/// Milliseconds since the Unix epoch.
pub(crate) fn milliseconds_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The file of a lease in a local store.
    fn lease_path(store: &Store, name: &str) -> std::path::PathBuf {
        store
            .local_path(&layout::lease(name).expect("a valid name"))
            .expect("a local store")
    }

    #[tokio::test]
    async fn one_holder_at_a_time_in_a_local_store() {
        let root = tempfile::tempdir().expect("a temporary directory");
        let store = Store::local(root.path()).expect("a store");
        let holder = Holder::new("run1", "P-1");
        let first = store.try_lease("tasks/default/ab", &holder).await;
        let first = first.expect("takes it").expect("a lease");
        let second = store.try_lease("tasks/default/ab", &holder).await;
        assert!(second.expect("asks").is_none());
        let held = store.held_leases("tasks/").await.expect("lists");
        assert_eq!(held, std::slice::from_ref(&holder));
        // A lease that is not held can be removed, a held one not.
        assert_eq!(store.remove_free_leases().await.expect("removes"), 0);
        drop(first);
        assert!(store.held_leases("tasks/").await.expect("lists").is_empty());
        let third = store.try_lease("tasks/default/ab", &holder).await;
        let third = third.expect("takes it").expect("a lease");
        drop(third);
        assert_eq!(store.remove_free_leases().await.expect("removes"), 1);
        assert!(!lease_path(&store, "tasks/default/ab").exists());
    }

    #[tokio::test]
    async fn an_object_lease_that_ended_is_taken_over() {
        let store = Store::memory().expect("a store");
        let holder = Holder::new("run1", "P-1");
        let first = store.try_lease("tasks/default/ab", &holder).await;
        let first = first.expect("takes it").expect("a lease");
        assert!(
            (store.try_lease("tasks/default/ab", &holder).await)
                .expect("asks")
                .is_none()
        );
        assert_eq!(store.held_leases("tasks").await.expect("lists").len(), 1);
        // A run that crashed renews nothing: its lease ends, and another run takes it over.
        let location = layout::lease("tasks/default/ab").expect("a name");
        let ended = Record {
            holder: Holder::new("crashed", "P-1"),
            token: "old".into(),
            expires: milliseconds_now() - 1,
        };
        let bytes = serde_json::to_vec(&ended).expect("encodes");
        store
            .operator
            .write(&location, bytes)
            .await
            .expect("writes");
        assert!(store.held_leases("tasks").await.expect("lists").is_empty());
        let taken = store.try_lease("tasks/default/ab", &holder).await;
        let taken = taken.expect("takes it").expect("a lease");
        // The old holder gives back only its own lease.
        drop(first);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(store.held_leases("tasks").await.expect("lists").len(), 1);
        drop(taken);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(store.held_leases("tasks").await.expect("lists").is_empty());
    }
}
