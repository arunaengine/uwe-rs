//! Coordinates named store leases and tracks their ownership and expiry.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fs::{File, TryLockError};
use std::io::{ErrorKind as IoErrorKind, Read, Seek, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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

// Memory cannot share leases across hosts and has no conditional update/delete. Serialize
// only that service's lease operations here; remote services must provide conditional I/O.
static MEMORY_LEASES: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
    File {
        locked: File,
        path: std::path::PathBuf,
    },
    /// Dropping it stops the renewal, which then removes the object.
    Object {
        _renewal: oneshot::Sender<()>,
        operator: Operator,
        location: String,
        token: String,
        lost: Arc<AtomicBool>,
    },
}

impl Lease {
    /// Whether this lease is held by a non-expiring local file lock.
    pub fn is_local(&self) -> bool {
        matches!(&self._hold, Hold::File { .. })
    }

    /// Verifies current ownership; callers must check around operations protected by a lease.
    pub async fn check(&self) -> Result<(), StoreError> {
        let owned = match &self._hold {
            Hold::File { locked, path } => same_file(locked, path)?,
            Hold::Object {
                operator,
                location,
                token,
                lost,
                ..
            } => {
                let _serial = memory_serial(operator).await;
                let owned = !lost.load(Ordering::Acquire)
                    && read_record(operator, location)
                        .await?
                        .is_some_and(|(record, _)| {
                            record.token == *token && record.expires > milliseconds_now()
                        });
                if !owned {
                    lost.store(true, Ordering::Release);
                }
                owned
            }
        };
        if !owned {
            return Err(StoreError::Location(
                "lease ownership was lost or expired".into(),
            ));
        }
        Ok(())
    }
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
    /// Memory leases coordinate this process only; other object stores require conditional
    /// operations so that renewal and deletion cannot replace a successor's lease.
    pub async fn try_lease(
        &self,
        name: &str,
        holder: &Holder,
    ) -> Result<Option<Lease>, StoreError> {
        // Runs on other machines see the leases of the shared store of a store in layers.
        if let Some(shared) = &self.shared {
            return Box::pin(shared.try_lease(name, holder)).await;
        }
        let location = layout::lease(name)?;
        match self.local_path(&location) {
            Some(path) => {
                let holder = holder.clone();
                let named = path.clone();
                let file = blocking(move || lock_file(&path, Some(&holder))).await?;
                Ok(file.map(|file| Lease {
                    _hold: Hold::File {
                        locked: file,
                        path: named,
                    },
                }))
            }
            None => take_object(&self.operator, &location, holder).await,
        }
    }

    /// Takes the lease `name` together with its other shared holders, unless someone holds it
    /// alone. Only leases in a folder can be shared (see [`Store::local_leases`]).
    pub async fn try_shared_lease(&self, name: &str) -> Result<Option<Lease>, StoreError> {
        if let Some(shared) = &self.shared {
            return Box::pin(shared.try_shared_lease(name)).await;
        }
        let location = layout::lease(name)?;
        let Some(path) = self.local_path(&location) else {
            let message = "leases in an object store cannot be shared";
            return Err(opendal::Error::new(ErrorKind::Unsupported, message).into());
        };
        let named = path.clone();
        let file = blocking(move || lock_file(&path, None)).await?;
        Ok(file.map(|file| Lease {
            _hold: Hold::File {
                locked: file,
                path: named,
            },
        }))
    }

    /// Whether the leases of the store are locked files of this machine, which never expire.
    pub fn local_leases(&self) -> bool {
        match &self.shared {
            Some(shared) => shared.local_leases(),
            None => self.root.is_some(),
        }
    }

    /// Who holds the leases whose names start with `prefix`, like `runs/`, now.
    pub async fn held_leases(&self, prefix: &str) -> Result<Vec<Holder>, StoreError> {
        if let Some(shared) = &self.shared {
            return Box::pin(shared.held_leases(prefix)).await;
        }
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
        if let Some(shared) = &self.shared {
            return Box::pin(shared.remove_free_leases()).await;
        }
        let mut removed = 0;
        for object in self.objects(&format!("{}/", layout::LEASES)).await? {
            if object.folder {
                continue;
            }
            let free = match self.local_path(&object.location) {
                Some(path) => blocking(move || remove_free_file(&path)).await?,
                None => {
                    lease_backend(&self.operator)?;
                    let _serial = memory_serial(&self.operator).await;
                    let record = read_record(&self.operator, &object.location).await?;
                    if let Some((record, tag)) = record
                        && record.expires <= milliseconds_now()
                    {
                        delete_record(&self.operator, &object.location, tag.as_deref()).await?
                    } else {
                        false
                    }
                }
            };
            removed += usize::from(free);
        }
        Ok(removed)
    }
}

/// Locks the file at `path` for `holder` and writes who holds it into it; `None` when another
/// holds the lock. Without a holder the lock is shared with other shared holders.
fn lock_file(path: &Path, holder: Option<&Holder>) -> std::io::Result<Option<File>> {
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
        let locked = match holder {
            Some(_) => file.try_lock(),
            None => file.try_lock_shared(),
        };
        match locked {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Ok(None),
            Err(TryLockError::Error(error)) => return Err(error),
        }
        if !same_file(&file, path)? {
            continue;
        }
        // Shared holders leave the file as it is; only a sole holder names itself in it.
        if let Some(holder) = holder {
            file.set_len(0)?;
            file.rewind()?;
            file.write_all(&serde_json::to_vec(holder)?)?;
        }
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
    if !same_file(&file, path)? {
        return Ok(false);
    }
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
    lease_backend(operator)?;
    let _serial = memory_serial(operator).await;
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
    let taken = match tag {
        // Replaced only if it is still the lease that ended, so of two runs that take it over
        // at once, one wins.
        Some(tag) => operator.write_with(location, bytes).if_match(&tag).await,
        None => operator.write(location, bytes).await,
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
    let lost = Arc::new(AtomicBool::new(false));
    let lease = Lease {
        _hold: Hold::Object {
            _renewal: stop,
            operator: operator.clone(),
            location: location.clone(),
            token: record.token.clone(),
            lost: lost.clone(),
        },
    };
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut stopped => break,
                () = tokio::time::sleep(RENEWAL) => {}
            }
            // A holder that was held up for longer than the lease lost it; it must not take it
            // back from the run that took it over.
            let _serial = memory_serial(&operator).await;
            if lost.load(Ordering::Acquire) {
                return;
            }
            let tag = match read_record(&operator, &location).await {
                Ok(Some((found, tag)))
                    if found.token == record.token && found.expires > milliseconds_now() =>
                {
                    tag
                }
                _ => {
                    lost.store(true, Ordering::Release);
                    return;
                }
            };
            record.expires = milliseconds_now() + LEASE_TIME.as_millis() as i64;
            let bytes = serde_json::to_vec(&record).expect("lease record serializes");
            let renewed = match tag {
                Some(tag) => operator.write_with(&location, bytes).if_match(&tag).await,
                None => operator.write(&location, bytes).await,
            };
            if renewed.is_err() {
                lost.store(true, Ordering::Release);
                return;
            }
        }
        let _serial = memory_serial(&operator).await;
        if let Ok(Some((found, tag))) = read_record(&operator, &location).await
            && found.token == record.token
        {
            let _ = delete_record(&operator, &location, tag.as_deref()).await;
        }
    });
    lease
}

fn lease_backend(operator: &Operator) -> Result<(), StoreError> {
    let capability = operator.info().capability();
    if operator.info().scheme() != "memory"
        && !(capability.read_with_if_match
            && capability.write_with_if_match
            && capability.write_with_if_not_exists
            && capability.delete_with_if_match)
    {
        return Err(opendal::Error::new(
            ErrorKind::Unsupported,
            "remote leases require conditional read, creation, renewal and deletion",
        )
        .into());
    }
    Ok(())
}

async fn memory_serial(operator: &Operator) -> Option<tokio::sync::MutexGuard<'static, ()>> {
    if operator.info().scheme() == "memory" {
        Some(MEMORY_LEASES.lock().await)
    } else {
        None
    }
}

async fn delete_record(
    operator: &Operator,
    location: &str,
    tag: Option<&str>,
) -> Result<bool, StoreError> {
    let deleted = match tag {
        Some(tag) => operator.delete_with(location).if_match(tag).await,
        None => operator.delete(location).await,
    };
    match deleted {
        Ok(()) => Ok(true),
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::ConditionNotMatch | ErrorKind::NotFound
            ) =>
        {
            Ok(false)
        }
        Err(error) => Err(error.into()),
    }
}

/// The lease object at `location` with its entity tag; `None` when there is none. An object
/// that cannot be read as a lease counts as one that has ended.
async fn read_record(
    operator: &Operator,
    location: &str,
) -> Result<Option<(Record, Option<String>)>, StoreError> {
    lease_backend(operator)?;
    let mut attempts = 0;
    loop {
        attempts += 1;
        let tag = match operator.stat(location).await {
            Ok(metadata) => metadata.etag().map(str::to_owned),
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let read = match tag.as_deref() {
            Some(tag) => operator.read_with(location).if_match(tag).await,
            None if operator.info().scheme() == "memory" => operator.read(location).await,
            None => {
                return Err(opendal::Error::new(
                    ErrorKind::Unsupported,
                    "remote lease metadata has no entity tag",
                )
                .into());
            }
        };
        let bytes = match read {
            Ok(buffer) => buffer.to_bytes(),
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) if error.kind() == ErrorKind::ConditionNotMatch && attempts < 4 => continue,
            Err(error) => return Err(error.into()),
        };
        let record = serde_json::from_slice(&bytes).unwrap_or_else(|_| Record {
            holder: Holder::default(),
            token: String::new(),
            expires: 0,
        });
        return Ok(Some((record, tag)));
    }
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
    use opendal::OperationContext;
    use opendal::raw::*;
    use std::sync::atomic::AtomicUsize;

    #[derive(Debug)]
    struct ConditionalRead {
        inner: Servicer,
        reads: AtomicUsize,
        stats: AtomicUsize,
        version: AtomicUsize,
        failures: usize,
        error: ErrorKind,
    }

    impl Service for ConditionalRead {
        type Reader = oio::Reader;
        type Writer = oio::Writer;
        type Lister = oio::Lister;
        type Deleter = oio::Deleter;
        type Copier = oio::Copier;
        type Composer = oio::Composer;

        fn info(&self) -> ServiceInfo {
            self.inner.info()
        }
        fn capability(&self) -> opendal::Capability {
            let mut capability = self.inner.capability();
            capability.read_with_if_match = true;
            capability
        }
        async fn create_dir(
            &self,
            ctx: &OperationContext,
            path: &str,
            args: OpCreateDir,
        ) -> opendal::Result<RpCreateDir> {
            self.inner.create_dir(ctx, path, args).await
        }
        async fn stat(
            &self,
            ctx: &OperationContext,
            path: &str,
            args: OpStat,
        ) -> opendal::Result<RpStat> {
            self.stats.fetch_add(1, Ordering::Relaxed);
            let tag = format!("v{}", self.version.load(Ordering::Relaxed));
            Ok(self
                .inner
                .stat(ctx, path, args)
                .await?
                .map_metadata(|metadata| {
                    let mut metadata = metadata.into_builder();
                    metadata.etag(tag);
                    metadata.build()
                }))
        }
        fn read(
            &self,
            ctx: &OperationContext,
            path: &str,
            args: OpRead,
        ) -> opendal::Result<Self::Reader> {
            let read = self.reads.fetch_add(1, Ordering::Relaxed);
            if read < self.failures {
                self.version.fetch_add(1, Ordering::Relaxed);
                return Err(opendal::Error::new(self.error, "injected version change"));
            }
            let tag = format!("v{}", self.version.load(Ordering::Relaxed));
            if args.if_match() != Some(tag.as_str()) {
                return Err(opendal::Error::new(
                    ErrorKind::ConditionNotMatch,
                    "stale conditional read",
                ));
            }
            self.inner.read(ctx, path, OpRead::new())
        }
        fn write(
            &self,
            ctx: &OperationContext,
            path: &str,
            args: OpWrite,
        ) -> opendal::Result<Self::Writer> {
            self.inner.write(ctx, path, args)
        }
        fn delete(&self, ctx: &OperationContext) -> opendal::Result<Self::Deleter> {
            self.inner.delete(ctx)
        }
        fn list(
            &self,
            ctx: &OperationContext,
            path: &str,
            args: OpList,
        ) -> opendal::Result<Self::Lister> {
            self.inner.list(ctx, path, args)
        }
        fn copy(
            &self,
            ctx: &OperationContext,
            from: &str,
            to: &str,
            args: OpCopy,
        ) -> opendal::Result<Self::Copier> {
            self.inner.copy(ctx, from, to, args)
        }
        fn compose(
            &self,
            ctx: &OperationContext,
            to: &str,
            args: OpCompose,
        ) -> opendal::Result<Self::Composer> {
            self.inner.compose(ctx, to, args)
        }
        async fn rename(
            &self,
            ctx: &OperationContext,
            from: &str,
            to: &str,
            args: OpRename,
        ) -> opendal::Result<RpRename> {
            self.inner.rename(ctx, from, to, args).await
        }
        async fn presign(
            &self,
            ctx: &OperationContext,
            path: &str,
            args: OpPresign,
        ) -> opendal::Result<RpPresign> {
            self.inner.presign(ctx, path, args).await
        }
    }

    async fn conditional_lease(
        token: &str,
        expires: i64,
        failures: usize,
        error: ErrorKind,
    ) -> (Lease, Arc<ConditionalRead>, oneshot::Receiver<()>) {
        let inner = Operator::new(opendal::services::Memory::default()).unwrap();
        let location = "locks/test";
        let record = Record {
            holder: Holder::new("run", "task"),
            token: token.into(),
            expires,
        };
        inner
            .write(location, serde_json::to_vec(&record).unwrap())
            .await
            .unwrap();
        let service = Arc::new(ConditionalRead {
            inner: inner.service().clone(),
            reads: AtomicUsize::new(0),
            stats: AtomicUsize::new(0),
            version: AtomicUsize::new(0),
            failures,
            error,
        });
        let operator = Operator::from_parts(inner.context().clone(), service.clone());
        let (stop, stopped) = oneshot::channel();
        let lease = Lease {
            _hold: Hold::Object {
                _renewal: stop,
                operator,
                location: location.into(),
                token: "owner".into(),
                lost: Arc::new(AtomicBool::new(false)),
            },
        };
        (lease, service, stopped)
    }

    #[tokio::test]
    async fn refreshes_lease_versions() {
        for failures in [0, 1, 3] {
            let (lease, service, _stopped) = conditional_lease(
                "owner",
                milliseconds_now() + 60_000,
                failures,
                ErrorKind::ConditionNotMatch,
            )
            .await;
            tokio::time::timeout(Duration::from_secs(1), lease.check())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(service.reads.load(Ordering::Relaxed), failures + 1);
            assert_eq!(service.stats.load(Ordering::Relaxed), failures + 1);
            lease.check().await.unwrap();
        }
        for (token, expires) in [("successor", milliseconds_now() + 60_000), ("owner", 0)] {
            let (lease, service, _stopped) =
                conditional_lease(token, expires, 1, ErrorKind::ConditionNotMatch).await;
            assert!(lease.check().await.is_err());
            assert_eq!(service.reads.load(Ordering::Relaxed), 2);
            assert!(lease.check().await.is_err());
            assert_eq!(service.reads.load(Ordering::Relaxed), 2);
        }
        for (error, expected_reads) in [
            (ErrorKind::ConditionNotMatch, 4),
            (ErrorKind::PermissionDenied, 1),
        ] {
            let (lease, service, _stopped) =
                conditional_lease("owner", milliseconds_now() + 60_000, 100, error).await;
            let failure = tokio::time::timeout(Duration::from_secs(1), lease.check())
                .await
                .unwrap()
                .unwrap_err();
            assert!(matches!(failure, StoreError::Backend(found) if found.kind() == error));
            assert_eq!(service.reads.load(Ordering::Relaxed), expected_reads);
            assert_eq!(service.stats.load(Ordering::Relaxed), expected_reads);
        }
    }

    /// The file of a lease in a local store.
    fn lease_path(store: &Store, name: &str) -> std::path::PathBuf {
        store
            .local_path(&layout::lease(name).expect("a valid name"))
            .expect("a local store")
    }

    #[tokio::test]
    async fn excludes_local_holders() {
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
        assert_eq!(store.remove_free_leases().await.expect("removes again"), 0);
    }

    #[tokio::test]
    async fn shares_local_leases() {
        let root = tempfile::tempdir().expect("a temporary directory");
        let store = Store::local(root.path()).expect("a store");
        assert!(store.local_leases());
        let first = store.try_shared_lease("gate").await.expect("takes it");
        let second = store.try_shared_lease("gate").await.expect("takes it");
        let (first, second) = (first.expect("a lease"), second.expect("a lease"));
        let sole = Holder::new("cleanup", "gate");
        assert!(
            store
                .try_lease("gate", &sole)
                .await
                .expect("asks")
                .is_none()
        );
        assert_eq!(store.remove_free_leases().await.expect("removes"), 0);
        drop(first);
        second.check().await.expect("still held");
        assert!(
            store
                .try_lease("gate", &sole)
                .await
                .expect("asks")
                .is_none()
        );
        drop(second);
        let alone = store.try_lease("gate", &sole).await.expect("takes it");
        assert!(alone.is_some());
        assert!(
            store
                .try_shared_lease("gate")
                .await
                .expect("asks")
                .is_none()
        );
        let memory = Store::memory().expect("a store");
        assert!(!memory.local_leases());
        assert!(memory.try_shared_lease("gate").await.is_err());
    }

    #[tokio::test]
    async fn reclaims_expired_leases() {
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
