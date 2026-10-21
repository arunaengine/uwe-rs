//! Completed image receipts use Store entries and leases without invoking a container engine.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use conda_environment::{
    BuiltImage, FORMAT, IMAGE_CACHE_NAMESPACE, ImageBuild, ImageCache, ImagePreparation, Lock,
};
use content_store::{Holder, Store, layout};
use task_model::Digest;

fn preparation(base: char) -> ImagePreparation {
    let lock = Lock {
        format: FORMAT,
        platform: "linux-64".into(),
        channels: vec![],
        specs: vec![],
        pip: vec![],
        pypi: None,
        packages: vec![],
        virtual_packages: Some(vec![]),
    };
    ImagePreparation::new(
        &lock,
        &format!("mambaorg/micromamba@sha256:{}", base.to_string().repeat(64)),
        "linux/amd64",
        &[],
    )
    .unwrap()
}

fn completed(
    preparation: &ImagePreparation,
    image: char,
    archive: Option<&Digest>,
) -> conda_environment::CompletedImage {
    let manifest = format!("sha256:{}", image.to_string().repeat(64));
    preparation
        .completed(BuiltImage {
            preparation: preparation.digest().into(),
            base: preparation.base().into(),
            recipe_digest: preparation.recipe_digest(),
            platform: preparation.platform().into(),
            image_id: format!("sha256:{}", image.to_string().repeat(64)),
            manifest_digest: Some(manifest.clone()),
            archive_digest: archive.map(ToString::to_string),
            reference: Some(format!("registry.example/conda@{manifest}")),
        })
        .unwrap()
}

fn key(preparation: &ImagePreparation) -> Digest {
    preparation.digest().parse().unwrap()
}

fn holder(name: &str) -> Holder {
    Holder::new(name, "build Conda image")
}

#[tokio::test]
async fn admits_one_builder() {
    let folder = tempfile::tempdir().unwrap();
    let cache = ImageCache::new(Store::local(folder.path()).unwrap());
    let preparation = preparation('a');
    let first_holder = holder("first");
    let second_holder = holder("second");
    let (first, second) = tokio::join!(
        cache.try_build(&preparation, &first_holder),
        cache.try_build(&preparation, &second_holder)
    );
    let outcomes = [first.unwrap(), second.unwrap()];
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, ImageBuild::Owner(_)))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, ImageBuild::Busy))
            .count(),
        1
    );
}

#[tokio::test]
async fn invalidates_observed_entries() {
    let store = Store::memory().unwrap();
    let cache = ImageCache::new(store.clone());
    let current = preparation('a');
    let cache_key = key(&current);

    store
        .put_entry(
            IMAGE_CACHE_NAMESPACE,
            &cache_key,
            b"not json".as_slice().into(),
        )
        .await
        .unwrap();
    assert!(cache.lookup(&current).await.unwrap().is_none());
    assert!(
        store
            .get_entry(IMAGE_CACHE_NAMESPACE, &cache_key)
            .await
            .unwrap()
            .is_none()
    );

    let other = preparation('b');
    let stale = serde_json::to_vec(&completed(&other, 'c', None)).unwrap();
    store
        .put_entry(IMAGE_CACHE_NAMESPACE, &cache_key, stale.into())
        .await
        .unwrap();
    assert!(cache.lookup(&current).await.unwrap().is_none());

    let missing: Digest = format!("sha256:{}", "d".repeat(64)).parse().unwrap();
    let stale = serde_json::to_vec(&completed(&current, 'c', Some(&missing))).unwrap();
    store
        .put_entry(IMAGE_CACHE_NAMESPACE, &cache_key, stale.into())
        .await
        .unwrap();
    assert!(cache.lookup(&current).await.unwrap().is_none());
}

#[tokio::test]
async fn preserves_replacement_entries() {
    let folder = tempfile::tempdir().unwrap();
    let store = Store::local(folder.path()).unwrap();
    let cache = ImageCache::new(store.clone());
    let preparation = preparation('a');
    let cache_key = key(&preparation);
    let first = completed(&preparation, 'b', None);
    store
        .put_entry(
            IMAGE_CACHE_NAMESPACE,
            &cache_key,
            serde_json::to_vec(&first).unwrap().into(),
        )
        .await
        .unwrap();
    let observed = cache.lookup(&preparation).await.unwrap().unwrap();
    let replacement = completed(&preparation, 'c', None);
    let path = store
        .local_path(&layout::entry(IMAGE_CACHE_NAMESPACE, &cache_key).unwrap())
        .unwrap();
    std::fs::write(path, serde_json::to_vec(&replacement).unwrap()).unwrap();
    assert!(!cache.invalidate(&preparation, &observed).await.unwrap());
    assert_eq!(
        cache.lookup(&preparation).await.unwrap().unwrap().receipt(),
        &replacement
    );
}

#[tokio::test]
async fn releases_unpublished_claims() {
    let folder = tempfile::tempdir().unwrap();
    let store = Store::local(folder.path()).unwrap();
    let cache = ImageCache::new(store);
    let preparation = preparation('a');
    let claim = match cache
        .try_build(&preparation, &holder("failed"))
        .await
        .unwrap()
    {
        ImageBuild::Owner(claim) => claim,
        _ => panic!("first build must own the preparation"),
    };
    drop(claim);
    assert!(matches!(
        cache
            .try_build(&preparation, &holder("retry"))
            .await
            .unwrap(),
        ImageBuild::Owner(_)
    ));
}

#[tokio::test]
async fn requires_publication_archive() {
    let store = Store::memory().unwrap();
    let cache = ImageCache::new(store.clone());
    let preparation = preparation('a');
    let claim = match cache
        .try_build(&preparation, &holder("owner"))
        .await
        .unwrap()
    {
        ImageBuild::Owner(claim) => claim,
        _ => panic!("a build claim"),
    };
    let absent: Digest = format!("sha256:{}", "e".repeat(64)).parse().unwrap();
    assert!(
        cache
            .publish(
                &preparation,
                &claim,
                &completed(&preparation, 'b', Some(&absent))
            )
            .await
            .is_err()
    );

    let archive = store
        .put_content(b"image archive".as_slice().into())
        .await
        .unwrap();
    let winner = completed(&preparation, 'c', Some(&archive));
    store
        .put_entry(
            IMAGE_CACHE_NAMESPACE,
            &key(&preparation),
            serde_json::to_vec(&winner).unwrap().into(),
        )
        .await
        .unwrap();
    let candidate = completed(&preparation, 'd', Some(&archive));
    assert_eq!(
        cache
            .recheck(&preparation, &claim)
            .await
            .unwrap()
            .unwrap()
            .receipt(),
        &winner
    );
    let found = cache
        .publish(&preparation, &claim, &candidate)
        .await
        .unwrap();
    assert_eq!(found.receipt(), &winner);
}

#[tokio::test]
async fn rejects_foreign_claims() {
    let first = ImageCache::new(Store::memory().unwrap());
    let second = ImageCache::new(Store::memory().unwrap());
    let preparation = preparation('a');
    let first_claim = match first
        .try_build(&preparation, &holder("first"))
        .await
        .unwrap()
    {
        ImageBuild::Owner(claim) => claim,
        _ => panic!("a build claim"),
    };
    let receipt = completed(&preparation, 'b', None);
    assert!(
        second
            .publish(&preparation, &first_claim, &receipt)
            .await
            .is_err()
    );
    let first_cached = first
        .publish(&preparation, &first_claim, &receipt)
        .await
        .unwrap();
    let second_claim = match second
        .try_build(&preparation, &holder("second"))
        .await
        .unwrap()
    {
        ImageBuild::Owner(claim) => claim,
        _ => panic!("a second build claim"),
    };
    second
        .publish(&preparation, &second_claim, &receipt)
        .await
        .unwrap();
    assert!(
        second
            .invalidate(&preparation, &first_cached)
            .await
            .is_err()
    );
    assert!(second.lookup(&preparation).await.unwrap().is_some());
}

#[tokio::test]
async fn prefers_shared_winner() {
    let shared = Store::memory().unwrap();
    let preparation = preparation('a');
    let cache_key = key(&preparation);
    let stale = completed(&preparation, 'b', None);
    let winner = completed(&preparation, 'c', None);
    let first_folder = tempfile::tempdir().unwrap();
    let second_folder = tempfile::tempdir().unwrap();
    let first = Store::local(first_folder.path()).unwrap();
    let second = Store::local(second_folder.path()).unwrap();
    for front in [&first, &second] {
        front
            .put_entry(
                IMAGE_CACHE_NAMESPACE,
                &cache_key,
                serde_json::to_vec(&stale).unwrap().into(),
            )
            .await
            .unwrap();
    }
    shared
        .put_entry(
            IMAGE_CACHE_NAMESPACE,
            &cache_key,
            serde_json::to_vec(&winner).unwrap().into(),
        )
        .await
        .unwrap();
    for front in [first, second] {
        let cache = ImageCache::new(Store::layered(front, shared.clone()).unwrap());
        assert_eq!(
            cache.lookup(&preparation).await.unwrap().unwrap().receipt(),
            &winner
        );
    }
}
