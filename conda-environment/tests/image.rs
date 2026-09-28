//! Fake package objects and independent tar decoding exercise image preparation, not builds.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::{self, Read, Write};

use conda_environment::{
    BuiltImage, CompletedImage, FORMAT, IMAGE_RECEIPT_FORMAT, ImagePreparation, Lock, Package,
    VirtualPackage, fetch_packages,
};
use sha2::{Digest as _, Sha256};

fn base() -> String {
    format!("mambaorg/micromamba@sha256:{}", "a".repeat(64))
}

fn capabilities() -> Vec<VirtualPackage> {
    vec![VirtualPackage {
        name: "__glibc".into(),
        version: "2.28".into(),
        build: "0".into(),
    }]
}

fn locked() -> Lock {
    let bytes = b"fake verified package";
    let digest: String = Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Lock {
        format: FORMAT,
        platform: "linux-64".into(),
        channels: vec!["A".into()],
        specs: vec!["foo".into()],
        pip: vec![],
        packages: vec![Package {
            name: "foo".into(),
            version: "1.0".into(),
            build: "0".into(),
            channel: "A".into(),
            subdir: "linux-64".into(),
            file: "foo-1.0-0.conda".into(),
            url: "https://example.org/foo-1.0-0.conda".into(),
            sha256: Some(digest),
            md5: None,
        }],
        virtual_packages: Some(capabilities()),
    }
}

fn built(preparation: &ImagePreparation) -> BuiltImage {
    let manifest = format!("sha256:{}", "b".repeat(64));
    BuiltImage {
        preparation: preparation.digest().into(),
        base: preparation.base().into(),
        recipe_digest: preparation.recipe_digest(),
        platform: preparation.platform().into(),
        image_id: format!("sha256:{}", "c".repeat(64)),
        manifest_digest: Some(manifest.clone()),
        archive_digest: Some(format!("sha256:{}", "d".repeat(64))),
        reference: Some(format!("registry.example/iunctus/conda@{manifest}")),
    }
}

#[test]
fn verifies_deterministic_context() {
    let cache = tempfile::tempdir().expect("package cache");
    let mut locked = locked();
    let mut second_package = locked.packages[0].clone();
    second_package.name = "bar".into();
    second_package.file = "bar-1.0-0.tar.bz2".into();
    locked.packages.push(second_package);
    fetch_packages(&locked, cache.path(), |_, out| {
        out.write_all(b"fake verified package")
    })
    .expect("verified objects");
    let preparation = ImagePreparation::new(&locked, &base(), "linux/amd64", &capabilities())
        .expect("image preparation");
    let mut first = Vec::new();
    preparation
        .write_context(cache.path(), &mut first)
        .expect("context");
    let mut second = Vec::new();
    preparation
        .write_context(cache.path(), &mut second)
        .expect("same context");
    assert_eq!(first, second);
    let mut archive = tar::Archive::new(first.as_slice());
    let mut decoded = Vec::new();
    for entry in archive.entries().expect("tar entries") {
        let mut entry = entry.expect("valid member");
        let name = entry.path().expect("path").to_string_lossy().into_owned();
        assert!(entry.header().entry_type().is_file());
        assert_eq!(entry.header().uid().expect("uid"), 0);
        assert_eq!(entry.header().gid().expect("gid"), 0);
        assert_eq!(entry.header().mtime().expect("mtime"), 0);
        assert_eq!(entry.header().mode().expect("mode"), 0o644);
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).expect("content");
        decoded.push((name, bytes));
    }
    assert_eq!(
        decoded
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        [
            "Dockerfile",
            "explicit.txt",
            "packages/0/foo-1.0-0.conda",
            "packages/1/bar-1.0-0.tar.bz2"
        ]
    );
    assert_eq!(decoded[0].1, preparation.recipe().as_bytes());
    assert!(
        preparation
            .recipe()
            .contains("micromamba create --yes --offline")
    );
    assert!(preparation.recipe().starts_with(&format!(
        "# iunctus Conda image recipe 1\nFROM --platform=linux/amd64 {}\n",
        base()
    )));
    assert_eq!(
        decoded[1].1,
        b"# platform: linux-64\n@EXPLICIT\nfile:///tmp/iunctus-conda-packages/0/foo-1.0-0.conda\nfile:///tmp/iunctus-conda-packages/1/bar-1.0-0.tar.bz2\n"
    );
    assert_eq!(decoded[2].1, b"fake verified package");
    assert_eq!(decoded[3].1, b"fake verified package");
    assert_eq!(preparation.base(), base());
    assert_eq!(preparation.platform(), "linux/amd64");
    let path = cache
        .path()
        .join(locked.packages[0].sha256.as_deref().expect("digest"));
    std::fs::write(path, b"fake corrupted bytes").expect("damage cache");
    assert!(
        preparation
            .write_context(cache.path(), &mut Vec::new())
            .is_err()
    );
    assert!(
        preparation
            .write_context(
                tempfile::tempdir().expect("empty cache").path(),
                &mut Vec::new()
            )
            .is_err()
    );
}

#[test]
fn binds_preparation_identity() {
    let mut locked = locked();
    let first =
        ImagePreparation::new(&locked, &base(), "linux/amd64", &capabilities()).expect("first");
    let same =
        ImagePreparation::new(&locked, &base(), "linux/amd64", &capabilities()).expect("same");
    assert_eq!(first.digest(), same.digest());
    let other_base = format!("mambaorg/micromamba@sha256:{}", "b".repeat(64));
    let changed = ImagePreparation::new(&locked, &other_base, "linux/amd64", &capabilities())
        .expect("base change");
    assert_ne!(first.digest(), changed.digest());
    locked.specs.push("bar".into());
    let changed = ImagePreparation::new(&locked, &base(), "linux/amd64", &capabilities())
        .expect("lock change");
    assert_ne!(first.digest(), changed.digest());
    let mut target = capabilities();
    target[0].version = "2.31".into();
    assert!(ImagePreparation::new(&locked, &base(), "linux/amd64", &target).is_err());
    locked.virtual_packages = Some(target.clone());
    let changed = ImagePreparation::new(&locked, &base(), "linux/amd64", &target)
        .expect("new capability lock");
    assert_ne!(first.digest(), changed.digest());
    locked.platform = "linux-aarch64".into();
    let arm =
        ImagePreparation::new(&locked, &base(), "linux/arm64", &target).expect("arm identity");
    let variant = ImagePreparation::new(&locked, &base(), "linux/arm64/v8", &target)
        .expect("variant identity");
    assert_ne!(arm.digest(), variant.digest());
    assert_ne!(arm.recipe(), variant.recipe());
}

#[test]
fn binds_receipt_provenance() {
    let preparation =
        ImagePreparation::new(&locked(), &base(), "linux/amd64", &capabilities()).unwrap();
    let receipt = preparation
        .completed(built(&preparation))
        .expect("a receipt");
    assert_eq!(receipt.format(), IMAGE_RECEIPT_FORMAT);
    assert_eq!(receipt.preparation(), preparation.digest());
    assert_eq!(receipt.base(), preparation.base());
    assert_eq!(receipt.recipe_digest(), preparation.recipe_digest());
    assert_eq!(receipt.platform(), preparation.platform());
    assert_eq!(receipt.image_id(), format!("sha256:{}", "c".repeat(64)));
    assert_eq!(
        receipt.manifest_digest(),
        Some(format!("sha256:{}", "b".repeat(64)).as_str())
    );
    assert_eq!(
        receipt.archive_digest(),
        Some(format!("sha256:{}", "d".repeat(64)).as_str())
    );
    receipt.validate(&preparation).expect("valid binding");
}

#[test]
fn rejects_mismatched_evidence() {
    let preparation =
        ImagePreparation::new(&locked(), &base(), "linux/amd64", &capabilities()).unwrap();
    for field in ["preparation", "base", "recipe", "platform"] {
        let mut image = built(&preparation);
        match field {
            "preparation" => image.preparation = format!("sha256:{}", "e".repeat(64)),
            "base" => image.base = format!("other/base@sha256:{}", "a".repeat(64)),
            "recipe" => image.recipe_digest = format!("sha256:{}", "e".repeat(64)),
            "platform" => image.platform = "linux/arm64".into(),
            _ => unreachable!(),
        }
        assert!(preparation.completed(image).is_err(), "accepted {field}");
    }
}

#[test]
fn requires_canonical_digests() {
    let preparation =
        ImagePreparation::new(&locked(), &base(), "linux/amd64", &capabilities()).unwrap();
    for field in ["image", "manifest", "archive"] {
        for invalid in [
            format!("sha256:{}", "A".repeat(64)),
            "a".repeat(64),
            format!("sha256:{}", "a".repeat(63)),
        ] {
            let mut image = built(&preparation);
            match field {
                "image" => image.image_id = invalid,
                "manifest" => image.manifest_digest = Some(invalid),
                "archive" => image.archive_digest = Some(invalid),
                _ => unreachable!(),
            }
            assert!(preparation.completed(image).is_err(), "accepted {field}");
        }
    }
    let mut image = built(&preparation);
    image.reference = Some(format!(
        "registry.example/iunctus/conda@sha256:{}",
        "e".repeat(64)
    ));
    assert!(preparation.completed(image).is_err());
    let mut image = built(&preparation);
    image.reference = Some("registry.example/iunctus/conda:latest".into());
    assert!(preparation.completed(image).is_err());
    let mut image = built(&preparation);
    image.manifest_digest = None;
    assert!(preparation.completed(image).is_err());
}

#[test]
fn preserves_receipt_encoding() {
    let preparation =
        ImagePreparation::new(&locked(), &base(), "linux/amd64", &capabilities()).unwrap();
    let receipt = preparation.completed(built(&preparation)).unwrap();
    let first = serde_json::to_vec(&receipt).unwrap();
    let decoded: CompletedImage = serde_json::from_slice(&first).unwrap();
    decoded.validate(&preparation).unwrap();
    assert_eq!(serde_json::to_vec(&decoded).unwrap(), first);

    let mut unknown: serde_json::Value = serde_json::from_slice(&first).unwrap();
    unknown["unexpected"] = serde_json::json!(true);
    assert!(serde_json::from_value::<CompletedImage>(unknown).is_err());

    let mut unsupported: serde_json::Value = serde_json::from_slice(&first).unwrap();
    unsupported["format"] = serde_json::json!(IMAGE_RECEIPT_FORMAT + 1);
    let unsupported: CompletedImage = serde_json::from_value(unsupported).unwrap();
    assert!(unsupported.validate(&preparation).is_err());
}

#[test]
fn rejects_unsafe_preparation() {
    let locked = locked();
    for reference in [
        "mambaorg/micromamba:latest".to_owned(),
        format!("bad\nRUN command@sha256:{}", "a".repeat(64)),
        format!("x@sha256:{}", "a".repeat(63)),
    ] {
        assert!(
            ImagePreparation::new(&locked, &reference, "linux/amd64", &capabilities()).is_err()
        );
    }
    assert!(ImagePreparation::new(&locked, &base(), "linux/arm64", &capabilities()).is_err());
    let mut invalid = locked.clone();
    invalid.virtual_packages = None;
    assert!(ImagePreparation::new(&invalid, &base(), "linux/amd64", &[]).is_err());
    invalid = locked.clone();
    invalid.pip.push("unpinned".into());
    assert!(ImagePreparation::new(&invalid, &base(), "linux/amd64", &capabilities()).is_err());
    for file in ["../foo.conda", "foo;command.conda", "foo.txt"] {
        invalid = locked.clone();
        invalid.packages[0].file = file.into();
        assert!(ImagePreparation::new(&invalid, &base(), "linux/amd64", &capabilities()).is_err());
    }
    invalid = locked.clone();
    invalid.packages[0].sha256 = None;
    assert!(ImagePreparation::new(&invalid, &base(), "linux/amd64", &capabilities()).is_err());
    invalid = locked.clone();
    invalid.packages.clear();
    invalid.virtual_packages = Some(vec![]);
    let preparation = ImagePreparation::new(&invalid, &base(), "linux/amd64", &[])
        .expect("authoritative absence");
    let mut context = Vec::new();
    preparation
        .write_context(tempfile::tempdir().expect("cache").path(), &mut context)
        .expect("empty environment");
    let mut archive = tar::Archive::new(context.as_slice());
    let directory = archive
        .entries()
        .expect("entries")
        .nth(2)
        .expect("packages member")
        .expect("directory");
    assert!(directory.header().entry_type().is_dir());
    assert_eq!(directory.header().mode().expect("directory mode"), 0o755);
    struct Failed;
    impl Write for Failed {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("sink failed"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    assert!(
        preparation
            .write_context(tempfile::tempdir().expect("cache").path(), &mut Failed)
            .is_err()
    );
}
