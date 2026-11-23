//! Collects garbage in a shared store only outside the grace window and active runs, and stops
//! its sweep when the lease or the epoch of the collection is lost.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use content_store::{GcPolicy, Holder, Object, RetentionDelegate, Store, StoreError, layout};
use futures_util::future::BoxFuture;
use task_runner::clean::{Keep, RemoteGc};

/// A store in layers with `count` contents that no run refers to. Its shared layer holds what
/// runs share, as a shared object store does, in a folder, which, unlike memory, tells when
/// each object was written.
async fn store(count: usize) -> (Store, tempfile::TempDir) {
    let folder = tempfile::tempdir().unwrap();
    let front = Store::local(&folder.path().join("objects")).unwrap();
    let shared = Store::local(&folder.path().join("shared")).unwrap();
    let store = Store::layered(front, shared).unwrap();
    for number in 0..count {
        let file = folder.path().join(format!("content-{number}"));
        std::fs::write(&file, format!("content {number}")).unwrap();
        store.put_file(&file).await.unwrap();
    }
    (store, folder)
}

/// The contents of the shared layer of a store in layers, else of the store.
async fn contents(store: &Store) -> usize {
    let objects = store
        .shared()
        .unwrap_or(store)
        .objects("cas/")
        .await
        .unwrap();
    objects.iter().filter(|object| !object.folder).count()
}

/// The contents of the folder in front of a store in layers.
async fn front(store: &Store) -> usize {
    let objects = store.front().unwrap().objects("cas/").await.unwrap();
    objects.iter().filter(|object| !object.folder).count()
}

fn collector(batch: usize) -> RemoteGc {
    let mut policy = GcPolicy::new(Duration::from_secs(2 * 3600)).unwrap();
    policy.batch = batch;
    RemoteGc::new(policy)
}

/// Three hours after now, past the grace window of [`collector`].
fn later() -> SystemTime {
    SystemTime::now() + Duration::from_secs(3 * 3600)
}

#[tokio::test]
async fn keeps_grace_window() {
    let (store, folder) = store(2).await;
    let gc = collector(64);
    let now = SystemTime::now();
    let memory = Store::memory().unwrap();
    let file = folder.path().join("unknown");
    std::fs::write(&file, "no write time").unwrap();
    memory.put_file(&file).await.unwrap();
    let removed = gc
        .collect(&memory, &Keep::default(), folder.path(), later())
        .await;
    assert_eq!(
        removed.unwrap().objects,
        0,
        "a store without write times keeps all"
    );
    let keep = Keep::default();
    let removed = gc.collect(&store, &keep, folder.path(), now).await;
    assert_eq!(removed.unwrap().objects, 0, "everything is recent");
    assert_eq!(contents(&store).await, 2);
    let before = front(&store).await;
    let removed = gc.collect(&store, &keep, folder.path(), later()).await;
    assert_eq!(removed.unwrap().objects, 2);
    assert_eq!(contents(&store).await, 0);
    assert_eq!(
        front(&store).await,
        before,
        "the front is evicted on its own"
    );
    let shared = store.shared().unwrap();
    assert_eq!(shared.epoch().await.unwrap().unwrap().number, 2);
}

#[tokio::test]
async fn keeps_active_runs() {
    let (store, folder) = store(2).await;
    let holder = Holder::new("run-a", "tasks");
    let lease = store.try_lease("runs/run-a", &holder).await.unwrap();
    assert!(lease.is_some());
    let (gc, keep) = (collector(64), Keep::default());
    let removed = gc.collect(&store, &keep, folder.path(), later()).await;
    assert_eq!(removed.unwrap().objects, 0, "a run reads the store");
    assert_eq!(contents(&store).await, 2);
}

/// Takes the objects it is given, and then, once, makes the collection lose what `losing` says,
/// or begins a run that reads the store.
struct Delegate {
    taken: Mutex<Vec<String>>,
    losing: Option<&'static str>,
    reader: tokio::sync::Mutex<Option<content_store::Lease>>,
}

impl RetentionDelegate for Delegate {
    fn release<'a>(
        &'a self,
        store: &'a Store,
        objects: &'a [Object],
    ) -> BoxFuture<'a, Result<usize, StoreError>> {
        Box::pin(async move {
            let first = {
                let mut taken = self.taken.lock().unwrap();
                let first = taken.is_empty();
                taken.extend(objects.iter().map(|object| object.location.clone()));
                first
            };
            match self.losing.filter(|_| first) {
                Some("epoch") => drop(store.advance_epoch("another collection").await?),
                Some("reader") => {
                    let holder = Holder::new("late", "run");
                    let lease = store.try_lease("runs/late", &holder).await?;
                    *self.reader.lock().await = Some(lease.expect("a free run lease"));
                }
                Some(_) => {
                    let shared = store.shared().unwrap_or(store);
                    shared.remove(&layout::lease("maintenance")?).await?;
                }
                None => {}
            }
            Ok(objects.len())
        })
    }
}

/// The result of a collection with a [`Delegate`], the delegate, and the store with its folder.
async fn delegated(
    losing: Option<&'static str>,
) -> (
    Result<usize, String>,
    Arc<Delegate>,
    (Store, tempfile::TempDir),
) {
    let (store, folder) = store(3).await;
    let delegate = Arc::new(Delegate {
        taken: Mutex::default(),
        losing,
        reader: tokio::sync::Mutex::default(),
    });
    let mut gc = collector(1);
    gc.delegate = Some(delegate.clone());
    let keep = Keep::default();
    let collected = gc.collect(&store, &keep, folder.path(), later()).await;
    let result = collected.map(|removed| removed.objects);
    (
        result.map_err(|error| error.to_string()),
        delegate,
        (store, folder),
    )
}

#[tokio::test]
async fn delegates_retention() {
    let (collected, delegate, (store, _folder)) = delegated(None).await;
    assert_eq!(collected, Ok(3));
    assert_eq!(delegate.taken.lock().unwrap().len(), 3);
    assert_eq!(
        contents(&store).await,
        3,
        "the host deletes them, uwe does not"
    );
}

#[tokio::test]
async fn stops_on_epoch() {
    let (collected, delegate, _) = delegated(Some("epoch")).await;
    let error = collected.expect_err("the sweep stops");
    assert!(error.contains("newer epoch"), "{error}");
    assert_eq!(delegate.taken.lock().unwrap().len(), 1, "one batch went");
}

#[tokio::test]
async fn stops_without_lease() {
    let (collected, delegate, _) = delegated(Some("lease")).await;
    let error = collected.expect_err("the sweep stops");
    assert!(error.contains("lease was lost"), "{error}");
    assert_eq!(delegate.taken.lock().unwrap().len(), 1, "one batch went");
}

/// A run that begins after the mark may reuse an old object that the mark found unused, so
/// the next batch does not go.
#[tokio::test]
async fn stops_for_readers() {
    let (collected, delegate, _) = delegated(Some("reader")).await;
    let error = collected.expect_err("the sweep stops");
    assert!(error.contains("a run began"), "{error}");
    assert_eq!(delegate.taken.lock().unwrap().len(), 1, "one batch went");
}
