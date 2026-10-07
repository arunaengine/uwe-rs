//! Checks historical lock bytes and versioned pip artifact identities.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use conda_environment::{Lock, PipLock, PipPackage};
use sha2::{Digest as _, Sha256};

#[test]
fn preserves_legacy_identity() {
    let bytes =
        br#"{"format":1,"platform":"linux-64","channels":[],"specs":[],"pip":[],"packages":[]}"#;
    let lock: Lock = serde_json::from_slice(bytes).unwrap();
    assert!(lock.pypi.is_none());
    assert_eq!(serde_json::to_vec(&lock).unwrap(), bytes);
    let digest: String = Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_eq!(lock.digest(), format!("sha256:{digest}"));
}

#[test]
fn pins_artifact_identity() {
    let mut lock: Lock = serde_json::from_str(
        r#"{"format":1,"platform":"linux-64","channels":[],"specs":[],"pip":["demo"],"packages":[]}"#,
    )
    .unwrap();
    let unpinned = lock.digest();
    lock.pypi = Some(PipLock {
        format: 1,
        python: "3.12.8".into(),
        packages: vec![PipPackage {
            name: "demo".into(),
            version: "1.0".into(),
            file: "demo-1.0-py3-none-any.whl".into(),
            url: "https://example.org/demo-1.0-py3-none-any.whl".into(),
            sha256: "a".repeat(64),
        }],
    });
    assert_ne!(lock.digest(), unpinned);
    let pinned = lock.digest();
    let restored: Lock = serde_json::from_slice(&serde_json::to_vec(&lock).unwrap()).unwrap();
    assert_eq!(restored, lock);
    lock.pypi.as_mut().unwrap().packages[0].sha256 = "b".repeat(64);
    assert_ne!(lock.digest(), pinned);
}
