//! Fake package objects and independent tar decoding exercise image preparation, not builds.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::{self, Read, Write};

use conda_environment::{FORMAT, ImagePreparation, Lock, Package, VirtualPackage, fetch_packages};
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

#[test]
fn tar_context_is_deterministic_offline_and_verified() {
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
fn identity_binds_lock_base_platform_recipe_and_target_capabilities() {
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
fn unsafe_or_unknown_preparation_is_refused_before_context() {
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
