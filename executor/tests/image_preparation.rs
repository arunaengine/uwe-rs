//! Checks native Docker build, load and import using tiny scratch images with cleanup.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeSet;
use std::panic::AssertUnwindSafe;

use executor::{CancellationToken, ContainerEngine, ImageSource};
use futures_util::{FutureExt, StreamExt};

#[tokio::test]
async fn prepares_scratch_images() {
    let engine = ContainerEngine::named(Some("docker"))
        .await
        .expect("local Docker");
    let root = tempfile::tempdir().unwrap();
    let token = root
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .replace('.', "");
    let tags = [
        format!("iunctus-cwl-test:build-{token}"),
        format!("iunctus-cwl-test:load-{token}"),
        format!("iunctus-cwl-test:import-{token}"),
        format!("iunctus-cwl-test:single-{token}"),
    ];
    let archive = root.path().join("context.tar");
    let mut builder = tar::Builder::new(std::fs::File::create(&archive).unwrap());
    for (name, bytes) in [
        ("Dockerfile", "FROM scratch\nCOPY payload /payload\n"),
        ("payload", token.as_str()),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, name, bytes.as_bytes())
            .unwrap();
    }
    builder.finish().unwrap();
    let result = AssertUnwindSafe(async {
        let cancel = CancellationToken::new();
        let id = engine
            .prepare_image(ImageSource::Build, &archive, &tags[0], None, &cancel)
            .await
            .unwrap();
        assert!(id.starts_with("sha256:"));
        assert_eq!(
            engine
                .prepare_image(ImageSource::Build, &archive, &tags[0], None, &cancel)
                .await
                .unwrap(),
            id
        );
        let mut bytes = Vec::new();
        let mut stream = engine.client().export_image(&id);
        while let Some(chunk) = stream.next().await {
            bytes.extend_from_slice(&chunk.unwrap());
            assert!(bytes.len() < 1 << 20, "scratch archive exceeds 1 MiB");
        }
        let saved = root.path().join("image.tar");
        std::fs::write(&saved, bytes).unwrap();
        assert_eq!(
            engine
                .prepare_image(ImageSource::Load, &saved, &tags[3], None, &cancel)
                .await
                .unwrap(),
            id
        );
        assert_eq!(
            engine
                .prepare_image(ImageSource::Load, &saved, &tags[1], Some(&tags[0]), &cancel)
                .await
                .unwrap(),
            id
        );
        let imported = engine
            .prepare_image(ImageSource::Import, &archive, &tags[2], None, &cancel)
            .await
            .unwrap();
        assert!(imported.starts_with("sha256:"));
        assert_eq!(
            engine
                .prepare_image(ImageSource::Import, &archive, &tags[2], None, &cancel)
                .await
                .unwrap(),
            imported
        );
    })
    .catch_unwind()
    .await;
    let mut images = BTreeSet::new();
    for tag in &tags {
        if let Ok(image) = engine.client().inspect_image(tag).await
            && let Some(id) = image.id
        {
            images.insert(id);
        }
    }
    for id in images {
        engine
            .client()
            .remove_image(
                &id,
                Some(bollard::query_parameters::RemoveImageOptions {
                    force: true,
                    ..Default::default()
                }),
                None,
            )
            .await
            .expect("remove the test image and all its tags");
    }
    result.unwrap();
}
