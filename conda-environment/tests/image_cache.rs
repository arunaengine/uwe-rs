//! Completed image receipts use Store entries and leases without invoking a container engine.
// Copyright (c) 2026 The iunctus Contributors
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
async fn concurrent_builders_have_one_owner() {
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
async fn publication_requires_its_archive_and_returns_the_stored_winner() {
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
async fn foreign_claims_and_observations_are_rejected() {
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

