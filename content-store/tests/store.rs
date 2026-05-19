//! Tests the store contract on a local directory, in memory and, when configured, on S3.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use bytes::Bytes;
use content_store::{S3Options, Store, StoreError, layout};
use task_model::{Digest, Tree, TreeEntry};

async fn check_contract(store: &Store) {
    // Contents are addressed by digest and stored once.
    let bytes = Bytes::from_static(b"reads\n");
    let digest = store.put_content(bytes.clone()).await.expect("put");
    assert_eq!(digest, Digest::sha256(b"reads\n"));
    assert_eq!(
        store.put_content(bytes.clone()).await.expect("put again"),
        digest
    );
    assert!(store.has_content(&digest).await.expect("head"));
    assert_eq!(store.get_content(&digest).await.expect("get"), bytes);
    let missing = Digest::sha256(b"missing");
    assert!(!store.has_content(&missing).await.expect("head"));

    // The first cache entry for a key wins; later writes do not replace it.
    let key = Digest::sha256(b"task");
    assert_eq!(store.get_entry("", &key).await.expect("get entry"), None);
    let first = Bytes::from_static(b"{\"first\":true}");
    assert!(
        store
            .put_entry("", &key, first.clone())
            .await
            .expect("put entry")
    );
    let second = Bytes::from_static(b"{\"first\":false}");
    assert!(!store.put_entry("", &key, second).await.expect("put entry"));
    assert_eq!(
        store.get_entry("", &key).await.expect("get entry"),
        Some(first)
    );

    // Outputs keep their original names under the attempt.
    let name = "results/sample 1.html";
    let location = store
        .put_output("run1", "fastqc-1", 1, name, Bytes::from_static(b"<html/>"))
        .await
        .expect("put output");
    assert_eq!(location, "runs/run1/fastqc-1/1/work/results/sample 1.html");
    let read = store.get_object(&location).await.expect("get output");
    assert_eq!(read, Bytes::from_static(b"<html/>"));

    // Directory manifests are addressed by the digest of their canonical form.
    let tree = Tree {
        entries: vec![
            TreeEntry::EmptyDirectory { path: "e".into() },
            TreeEntry::File {
                path: "a/b.txt".into(),
                digest: digest.clone(),
                size: 6,
                executable: false,
            },
        ],
    };
    let tree_digest = store.put_tree(&tree).await.expect("put tree");
    assert_eq!(Ok(tree_digest.clone()), tree.digest());
    let read = store.get_tree(&tree_digest).await.expect("get tree");
    assert_eq!(read.digest(), tree.digest());
}

#[tokio::test]
async fn memory_contract() {
    check_contract(&Store::memory().expect("memory store")).await;
}

#[tokio::test]
async fn local_contract() {
    let root = tempfile::tempdir().expect("temporary directory");
    let store = Store::local(root.path()).expect("local store");
    check_contract(&store).await;
    let digest = Digest::sha256(b"reads\n");
    assert!(root.path().join(layout::content(&digest)).is_file());
    let path = store.local_path(&layout::content(&digest));
    assert_eq!(path, Some(root.path().join(layout::content(&digest))));
    assert_eq!(
        Store::memory().expect("memory store").local_path("cas"),
        None
    );
}

#[tokio::test]
async fn detects_corruption() {
    let root = tempfile::tempdir().expect("temporary directory");
    let store = Store::local(root.path()).expect("local store");
    let digest = store
        .put_content(Bytes::from_static(b"good"))
        .await
        .expect("put");
    std::fs::write(root.path().join(layout::content(&digest)), b"bad").expect("overwrite");
    let error = store
        .get_content(&digest)
        .await
        .expect_err("corrupt content");
    assert!(matches!(error, StoreError::Corrupt { .. }), "{error}");
}

#[tokio::test]
async fn uploads_large_files() {
    let root = tempfile::tempdir().expect("temporary directory");
    let store = Store::local(&root.path().join("store")).expect("local store");
    // Larger than one part, so the upload needs more than one chunk.
    let content: Vec<u8> = (0..20 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
    let file = root.path().join("large.bin");
    std::fs::write(&file, &content).expect("write input");
    let (digest, size) = store.put_file(&file).await.expect("put file");
    assert_eq!(size, content.len() as u64);
    assert_eq!(digest, Digest::sha256(&content));
    assert_eq!(store.get_content(&digest).await.expect("get"), content);
    let location = store
        .put_output_file("run1", "task-1", 1, "/tmp/large.bin", &file)
        .await
        .expect("put output file");
    assert_eq!(location, "runs/run1/task-1/1/root/tmp/large.bin");
    let stored = store.local_path(&location).expect("a local store");
    assert_eq!(std::fs::read(stored).expect("read"), content);
}

#[test]
fn rejects_bad_locations() {
    let key = Digest::sha256(b"task");
    assert!(layout::entry("../escape", &key).is_err());
    assert!(layout::entry(".hidden", &key).is_err());
    assert!(layout::output("run", "task", 1, "../x").is_err());
    assert!(layout::output("run", "task", 1, "a//b").is_err());
    assert!(layout::output("run/other", "task", 1, "x").is_err());
    assert!(layout::output("run", "task", 1, "/").is_err());
    assert!(layout::output("run", "task", 1, "/a/../b").is_err());
    let digest = Digest::sha256(b"");
    assert_eq!(
        layout::content(&digest),
        "cas/sha256/e3/b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

/// Runs the contract on a real S3-compatible bucket, for example RustFS started by `just s3-server`.
#[tokio::test]
#[ignore = "needs an S3 bucket; set IUNCTUS_S3_BUCKET and AWS credentials, then run with --ignored"]
async fn s3_contract() {
    let bucket = std::env::var("IUNCTUS_S3_BUCKET").expect("IUNCTUS_S3_BUCKET is set");
    let unique = std::process::id().to_string();
    let options = S3Options {
        bucket,
        prefix: Some(format!("iunctus-test-{unique}")),
        endpoint: std::env::var("IUNCTUS_S3_ENDPOINT").ok(),
        region: Some(std::env::var("AWS_REGION").unwrap_or_else(|_| "us-east-1".into())),
    };
    check_contract(&Store::s3(&options).expect("s3 store")).await;
}
