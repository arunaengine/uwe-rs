//! Fake transports exercise verified cache publication without host Conda or network.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io;

use conda_environment::{Environment, fetch_packages, fetch_repodata, lock};
use serde_json::json;
use sha2::{Digest as _, Sha256};

#[test]
fn repodata_transport_uses_expanded_channels_and_preserves_prior_bytes_on_failure() {
    let folder = tempfile::tempdir().expect("a cache");
    let environment = Environment::parse("channels:\n  - defaults\ndependencies:\n  - foo\n")
        .expect("an environment");
    let mut requested = Vec::new();
    fetch_repodata(&environment, folder.path(), "linux-64", |url, out| {
        requested.push(url.to_owned());
        out.write_all(b"{\"packages\":{}}")
    })
    .expect("fetch repodata");
    assert_eq!(
        requested,
        [
            "https://repo.anaconda.com/pkgs/main/linux-64/repodata.json",
            "https://repo.anaconda.com/pkgs/main/noarch/repodata.json",
            "https://repo.anaconda.com/pkgs/r/linux-64/repodata.json",
            "https://repo.anaconda.com/pkgs/r/noarch/repodata.json",
        ]
    );
    let path = folder.path().join("pkgs/main/linux-64/repodata.json");
    for transport_error in [false, true] {
        assert!(
            fetch_repodata(&environment, folder.path(), "linux-64", |_, out| {
                out.write_all(b"{\"packages\":{\"bad\":{\"name\":5}}}")?;
                if transport_error {
                    Err(io::Error::other("disconnected"))
                } else {
                    Ok(())
                }
            })
            .is_err()
        );
        assert_eq!(
            std::fs::read(&path).expect("prior data"),
            b"{\"packages\":{}}"
        );
        assert_eq!(
            std::fs::read_dir(path.parent().expect("parent"))
                .expect("cache")
                .count(),
            1
        );
    }
    for invalid in ["../../outside", "/linux-64", "linux/amd64"] {
        assert!(
            fetch_repodata(&environment, folder.path(), invalid, |_, _| {
                panic!("invalid paths must fail before transport")
            })
            .is_err()
        );
    }
    let invalid = Environment::parse("channels:\n  - ../outside\n").expect("an environment");
    assert!(
        fetch_repodata(&invalid, folder.path(), "linux-64", |_, _| {
            panic!("invalid channel must fail before transport")
        })
        .is_err()
    );
}

#[test]
fn packages_are_streamed_verified_reused_and_repaired() {
    let folder = tempfile::tempdir().expect("repodata");
    let cache = tempfile::tempdir().expect("package cache");
    let environment =
        Environment::parse("channels:\n  - A\ndependencies:\n  - foo\n").expect("an environment");
    let bytes = b"verified Conda package fixture";
    let digest: String = Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let repodata = json!({"packages": {"foo-1.0-0.conda": {
        "name":"foo", "version":"1.0", "build":"0", "sha256":digest
    }}})
    .to_string();
    fetch_repodata(&environment, folder.path(), "linux-64", |url, out| {
        out.write_all(if url.contains("/noarch/") {
            b"{}"
        } else {
            repodata.as_bytes()
        })
    })
    .expect("repodata");
    let mut locked = lock(&environment, folder.path(), "linux-64").expect("lock");
    let paths = fetch_packages(&locked, cache.path(), |url, out| {
        assert_eq!(url, "https://conda.anaconda.org/A/linux-64/foo-1.0-0.conda");
        for chunk in bytes.chunks(3) {
            out.write_all(chunk)?;
        }
        Ok(())
    })
    .expect("verified package");
    assert_eq!(paths, [cache.path().join(&digest)]);
    assert_eq!(std::fs::read(&paths[0]).expect("package"), bytes);
    assert_eq!(
        fetch_packages(&locked, cache.path(), |_, _| {
            panic!("verified cache should be reused")
        })
        .expect("cache hit"),
        paths
    );
    std::fs::write(&paths[0], b"corrupt cache").expect("damage cache");
    assert!(
        fetch_packages(&locked, cache.path(), |_, out| {
            out.write_all(b"partial download")?;
            Err(io::Error::other("disconnected"))
        })
        .is_err()
    );
    assert!(
        fetch_packages(&locked, cache.path(), |_, out| out
            .write_all(b"wrong download"))
        .is_err()
    );
    assert_eq!(
        std::fs::read(&paths[0]).expect("old cache"),
        b"corrupt cache"
    );
    assert_eq!(std::fs::read_dir(cache.path()).expect("cache").count(), 1);
    fetch_packages(&locked, cache.path(), |_, out| out.write_all(bytes)).expect("repaired cache");
    assert_eq!(std::fs::read(&paths[0]).expect("repaired"), bytes);
    locked.packages[0].sha256 = Some(digest.to_ascii_uppercase());
    assert_eq!(
        fetch_packages(&locked, cache.path(), |_, _| {
            panic!("uppercase digest should use the same verified cache")
        })
        .expect("normalized cache hit"),
        paths
    );
    for unsupported in [None, Some("../escape".into()), Some("a".repeat(63))] {
        locked.packages[0].sha256 = unsupported;
        locked.packages[0].md5 = Some("0123456789abcdef0123456789abcdef".into());
        assert!(
            fetch_packages(&locked, cache.path(), |_, _| {
                panic!("unsupported digest must fail before transport")
            })
            .is_err()
        );
    }
}
