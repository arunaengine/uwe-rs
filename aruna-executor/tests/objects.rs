//! Result bundles preserve wrapper records and reject filesystem escapes.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use aruna_executor::objects::collect;

fn bundle(path: &str, kind: tar::EntryType, contents: &[u8]) -> Vec<u8> {
    let mut archive = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_path(path).unwrap();
    header.set_mode(0o644);
    header.set_entry_type(kind);
    header.set_size(contents.len() as u64);
    header.set_cksum();
    archive.append(&header, contents).unwrap();
    archive.into_inner().unwrap()
}

#[test]
fn links_devices_and_unrelated_roots_are_refused() {
    for (path, kind) in [
        ("control/link", tar::EntryType::Symlink),
        ("outbox/link", tar::EntryType::Link),
        ("outbox/device", tar::EntryType::Char),
        ("remote.json", tar::EntryType::Regular),
    ] {
        let root = tempfile::tempdir().unwrap();
        assert!(collect(&bundle(path, kind, b""), root.path()).is_err());
        assert!(!root.path().join(path).exists());
    }
    for path in ["/control/escape", "outbox/../escape"] {
        let mut bytes = bundle("control/placeholder", tar::EntryType::Regular, b"");
        bytes[..100].fill(0);
        bytes[..path.len()].copy_from_slice(path.as_bytes());
        bytes[148..156].fill(b' ');
        let checksum: u32 = bytes[..512].iter().map(|byte| *byte as u32).sum();
        bytes[148..156].copy_from_slice(format!("{checksum:06o}\0 ").as_bytes());
        assert!(collect(&bytes, tempfile::tempdir().unwrap().path()).is_err());
    }
}

#[test]
fn success_requires_exit_and_manifest_records() {
    let root = tempfile::tempdir().unwrap();
    assert!(
        collect(
            &bundle("outbox", tar::EntryType::Directory, b""),
            root.path()
        )
        .is_err()
    );
}
