//! Persists local container ownership until deletion is confirmed.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, TryLockError};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use bollard::Docker;
use bollard::errors::Error as ApiError;
use serde::{Deserialize, Serialize};

pub(crate) const OWNER: &str = "org.uwe.create";

pub(crate) struct Journal {
    root: PathBuf,
    socket: String,
    daemon: String,
    active: Mutex<BTreeMap<String, Pending>>,
}

pub(crate) struct Pending {
    folder: PathBuf,
    _lock: File,
    record: Record,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    socket: String,
    daemon: String,
    name: String,
    owner: String,
    id: Option<String>,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Queue {
    next: u64,
    cursor: u64,
}

impl Journal {
    #[cfg(test)]
    pub(crate) fn empty(&self) -> bool {
        !self.root.join("0/record.json").exists()
    }
    pub(crate) fn new(base: &Path, socket: String, daemon: String) -> Self {
        let key = task_model::Digest::sha256(format!("{socket}\0{daemon}").as_bytes());
        let root = if base.as_os_str().is_empty() {
            PathBuf::new()
        } else if base.is_absolute() {
            base.join(key.hex())
        } else {
            std::env::current_dir()
                .unwrap_or_default()
                .join(base)
                .join(key.hex())
        };
        Self {
            root,
            socket,
            daemon,
            active: Mutex::default(),
        }
    }

    pub(crate) fn at(&self, base: &Path) -> Self {
        Self::new(base, self.socket.clone(), self.daemon.clone())
    }

    pub(crate) async fn verify(&self, client: &Docker) -> Result<(), String> {
        verify(client, &self.socket, &self.daemon).await
    }

    pub(crate) async fn begin(&self, name: &str, owner: &str) -> Result<Pending, String> {
        if self.daemon.is_empty() {
            return Err("the engine did not report its immutable daemon ID".into());
        }
        if !self.root.is_absolute() {
            return Err("container creation requires a persistent operator cache directory".into());
        }
        let record = Record {
            socket: self.socket.clone(),
            daemon: self.daemon.clone(),
            name: name.into(),
            owner: owner.into(),
            id: None,
        };
        let root = self.root.clone();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        tokio::task::spawn_blocking(move || {
            if let Err(Ok(pending)) = sender.send(create_record(&root, record)) {
                let _ = pending.reject();
            }
        });
        receiver.await.map_err(|error| error.to_string())?
    }

    pub(crate) fn bind(
        &self,
        mut pending: Pending,
        id: &str,
    ) -> Result<(), Box<(String, Pending)>> {
        pending.record.id = Some(id.into());
        if let Err(error) = pending.save() {
            return Err(Box::new((error, pending)));
        }
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if active.contains_key(id) {
            return Err(Box::new((
                "the engine reused a live container ID".into(),
                pending,
            )));
        }
        active.insert(id.into(), pending);
        Ok(())
    }

    pub(crate) fn release(&self, id: &str, removed: bool) {
        let pending = self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(id);
        if removed
            && let Some(pending) = pending
            && let Err(error) = pending.finish()
        {
            eprintln!("uwe: warning: retained container cleanup record: {error}");
        }
    }

    pub(crate) async fn recover(&self, client: &Docker) -> Result<(), String> {
        if !self.root.is_absolute() {
            return Ok(());
        }
        if !self.root.try_exists().map_err(|error| error.to_string())? {
            return Ok(());
        }
        let root = self.root.clone();
        let slots = tokio::task::spawn_blocking(move || {
            let _lock = queue_lock(&root)?;
            let mut queue = read_queue(&root)?;
            let start = if queue.cursor < queue.next {
                queue.cursor
            } else {
                0
            };
            let end = start.saturating_add(16).min(queue.next);
            queue.cursor = end;
            save_json(&root, "queue.json", &queue)?;
            Ok::<_, String>(start..end)
        })
        .await
        .map_err(|error| error.to_string())??;
        for slot in slots {
            let folder = self.root.join(slot.to_string());
            let lock = match File::options().write(true).open(folder.join("lock")) {
                Ok(lock) => lock,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.to_string()),
            };
            match lock.try_lock() {
                Ok(()) => {}
                Err(TryLockError::WouldBlock) => continue,
                Err(TryLockError::Error(error)) => return Err(error.to_string()),
            }
            let file = match File::open(folder.join("record.json")) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.to_string()),
            };
            let mut bytes = Vec::new();
            file.take(65_537)
                .read_to_end(&mut bytes)
                .map_err(|error| error.to_string())?;
            if bytes.len() > 65_536 {
                return Err("a container ownership record exceeds 64 KiB".into());
            }
            let record: Record =
                serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
            if record.socket != self.socket || record.daemon != self.daemon {
                return Err("a container ownership record has a different engine or owner".into());
            }
            let pending = Pending {
                folder,
                _lock: lock,
                record,
            };
            // A slow engine cannot turn historical cleanup into an unbounded admission wait.
            if tokio::time::timeout(Duration::from_secs(2), pending.reconcile(client))
                .await
                .is_err()
            {
                break;
            }
        }
        Ok(())
    }
}

fn create_record(root: &Path, record: Record) -> Result<Pending, String> {
    make_folder(root)?;
    let queue_held = queue_lock(root)?;
    let mut queue = read_queue(root)?;
    let folder = root.join(queue.next.to_string());
    queue.next = queue
        .next
        .checked_add(1)
        .ok_or("the container ownership queue is full")?;
    save_json(root, "queue.json", &queue)?;
    std::fs::create_dir(&folder).map_err(|error| error.to_string())?;
    File::open(root)
        .and_then(|file| file.sync_all())
        .map_err(|error| error.to_string())?;
    let lock = File::options()
        .write(true)
        .create_new(true)
        .open(folder.join("lock"))
        .map_err(|error| error.to_string())?;
    lock.try_lock().map_err(|error| error.to_string())?;
    // The held lock of the slot keeps recovery away from it, so the record needs no queue lock.
    drop(queue_held);
    let pending = Pending {
        folder,
        _lock: lock,
        record,
    };
    pending.save()?;
    Ok(pending)
}

/// How long a run waits for the queue of container records, which the runs of this machine hold
/// for a few writes each; on a loaded machine with many tasks starting, that takes a while.
const QUEUE_WAIT: Duration = Duration::from_secs(600);

/// How often a run that waits for the queue says so.
const QUEUE_NOTICE: Duration = Duration::from_secs(30);

fn queue_lock(root: &Path) -> Result<File, String> {
    let lock = File::options()
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join("queue.lock"))
        .map_err(|error| error.to_string())?;
    let started = std::time::Instant::now();
    let mut notice = QUEUE_NOTICE;
    loop {
        match lock.try_lock() {
            Ok(()) => return Ok(lock),
            Err(TryLockError::WouldBlock) if started.elapsed() < QUEUE_WAIT => {
                if started.elapsed() >= notice {
                    let waited = started.elapsed().as_secs();
                    tracing::warn!(waited, "waiting for the container ownership queue");
                    notice += QUEUE_NOTICE;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => {
                return Err(format!(
                    "cannot lock the container ownership queue: {error}"
                ));
            }
        }
    }
}

fn read_queue(root: &Path) -> Result<Queue, String> {
    let mut bytes = Vec::new();
    match File::open(root.join("queue.json")) {
        Ok(file) => {
            file.take(4097)
                .read_to_end(&mut bytes)
                .map_err(|error| error.to_string())?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Queue::default());
        }
        Err(error) => return Err(error.to_string()),
    }
    match bytes {
        bytes if bytes.len() <= 4096 => {
            serde_json::from_slice(&bytes).map_err(|error| error.to_string())
        }
        _ => Err("the container ownership queue exceeds 4 KiB".into()),
    }
}

impl Pending {
    fn save(&self) -> Result<(), String> {
        save_json(&self.folder, "record.json", &self.record)
    }

    pub(crate) fn reject(self) -> Result<(), String> {
        self.finish()
    }

    fn finish(self) -> Result<(), String> {
        std::fs::remove_file(self.folder.join("record.json")).map_err(|error| error.to_string())?;
        File::open(&self.folder)
            .and_then(|file| file.sync_all())
            .map_err(|error| error.to_string())?;
        std::fs::remove_file(self.folder.join("lock")).map_err(|error| error.to_string())?;
        std::fs::remove_dir(&self.folder).map_err(|error| error.to_string())?;
        File::open(
            self.folder
                .parent()
                .ok_or("an ownership record has no parent")?,
        )
        .and_then(|file| file.sync_all())
        .map_err(|error| error.to_string())
    }

    pub(crate) async fn reconcile(mut self, client: &Docker) -> bool {
        if verify(client, &self.record.socket, &self.record.daemon)
            .await
            .is_err()
        {
            return false;
        }
        let target = self.record.id.as_deref().unwrap_or(&self.record.name);
        let found = match client.inspect_container(target, None).await {
            Ok(found) => found,
            Err(ApiError::DockerResponseServerError {
                status_code: 404, ..
            }) if self.record.id.is_some() => {
                return self.finish().is_ok();
            }
            // Absence by name cannot settle a creation that the engine may still accept later.
            Err(_) => return false,
        };
        let owner = found
            .config
            .and_then(|config| config.labels)
            .and_then(|labels| labels.get(OWNER).cloned());
        let Some(id) = found.id.filter(|id| !id.is_empty()) else {
            return false;
        };
        if owner.as_deref() != Some(self.record.owner.as_str())
            || self
                .record
                .id
                .as_ref()
                .is_some_and(|expected| *expected != id)
        {
            return false;
        }
        self.record.id = Some(id.clone());
        if self.save().is_err() {
            return false;
        }
        super::remove(client, &id).await && self.finish().is_ok()
    }
}

/// The engines whose identity this process checked, by socket, daemon ID and the socket file:
/// an engine keeps its daemon ID, and a daemon that takes over the socket makes a new file.
static VERIFIED: Mutex<BTreeSet<(String, String, Endpoint)>> = Mutex::new(BTreeSet::new());

/// The device and inode of a socket file.
type Endpoint = (u64, u64);

/// The socket file of an engine, which only a new daemon replaces; none for an engine that is
/// no file, like one on the network, which is then asked every time.
fn endpoint(socket: &str) -> Option<Endpoint> {
    use std::os::unix::fs::MetadataExt;
    let found = std::fs::metadata(socket.trim_start_matches("unix://")).ok()?;
    Some((found.dev(), found.ino()))
}

/// How long one identity request may take, and how often it is made; an engine with a
/// hundred running containers answers slowly.
const IDENTITY_WAIT: Duration = Duration::from_secs(30);
const IDENTITY_TRIES: u32 = 3;

async fn verify(client: &Docker, socket: &str, daemon: &str) -> Result<(), String> {
    let changed =
        "the container engine identity changed; reconnect before creating or deleting containers";
    if daemon.is_empty() {
        return Err(changed.into());
    }
    let key = endpoint(socket).map(|file| (socket.to_owned(), daemon.to_owned(), file));
    let verified = || VERIFIED.lock().unwrap_or_else(PoisonError::into_inner);
    if key.as_ref().is_some_and(|key| verified().contains(key)) {
        return Ok(());
    }
    let mut failed = String::new();
    for attempt in 0..IDENTITY_TRIES {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        match tokio::time::timeout(IDENTITY_WAIT, client.info()).await {
            Ok(Ok(info)) if info.id.as_deref() == Some(daemon) => {
                verified().extend(key);
                return Ok(());
            }
            Ok(Ok(_)) => return Err(changed.into()),
            Ok(Err(error)) => failed = error.to_string(),
            Err(_) => failed = "the container engine identity request timed out".into(),
        }
    }
    Err(failed)
}

fn save_json(folder: &Path, name: &str, value: &impl Serialize) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    if bytes.len() > 65_536 {
        return Err("a container ownership record exceeds 64 KiB".into());
    }
    let mut file = tempfile::NamedTempFile::new_in(folder).map_err(|error| error.to_string())?;
    std::io::Write::write_all(file.as_file_mut(), &bytes).map_err(|error| error.to_string())?;
    file.as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    file.persist(folder.join(name))
        .map_err(|error| error.to_string())?;
    File::open(folder)
        .and_then(|file| file.sync_all())
        .map_err(|error| error.to_string())
}

fn make_folder(path: &Path) -> Result<(), String> {
    if path.is_dir() {
        return Ok(());
    }
    let parent = path
        .parent()
        .ok_or("the ownership directory has no parent")?;
    make_folder(parent)?;
    match std::fs::create_dir(path) {
        Ok(()) => File::open(parent)
            .and_then(|file| file.sync_all())
            .map_err(|error| error.to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(test)]
#[path = "journal_tests.rs"]
mod tests;
