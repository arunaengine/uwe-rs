//! Content identifiers and DAG-CBOR against the blocks that nf-blocks 0.11.0-beta.1 wrote,
//! byte for byte.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use storage::cbor::{Cid, DAG_CBOR, Node, RAW, decode, encode};

fn recorded() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cas_store")
}

/// A value as the decoder of the fixtures wrote it, links as `{"/": <cid>}`.
fn json(node: &Node) -> Value {
    match node {
        Node::Null => Value::Null,
        Node::Bool(flag) => json!(flag),
        Node::Integer(number) => json!(i64::try_from(*number).expect("a small integer")),
        Node::Float(number) => json!(number),
        Node::Text(text) => json!(text),
        Node::Bytes(bytes) => json!(bytes),
        Node::List(items) => Value::Array(items.iter().map(json).collect()),
        Node::Map(entries) => Value::Object(
            (entries.iter())
                .map(|(key, value)| (key.clone(), json(value)))
                .collect(),
        ),
        Node::Link(cid) => json!({ "/": cid.to_string() }),
    }
}

#[test]
fn matches_recorded_blocks() {
    let decoded: Vec<Value> =
        serde_json::Deserializer::from_str(include_str!("fixtures/cas_blocks.json"))
            .into_iter()
            .collect::<Result<_, _>>()
            .expect("the decoded blocks");
    let (mut raw, mut blocks) = (0, 0);
    for folder in std::fs::read_dir(recorded().join("blocks")).expect("the blocks") {
        let folder = folder.expect("a folder").path();
        for file in std::fs::read_dir(&folder).expect("a folder") {
            let file = file.expect("a block").path();
            let name = file
                .file_name()
                .expect("a name")
                .to_string_lossy()
                .into_owned();
            let cid = Cid::parse(&name).expect("a CID");
            assert_eq!(cid.to_string(), name);
            assert!(folder.ends_with(&name[name.len() - 2..]), "{name}");
            let bytes = std::fs::read(&file).expect("the block");
            assert_eq!(Cid::of(cid.codec, &bytes), cid, "{name}");
            if cid.codec == RAW {
                raw += 1;
                continue;
            }
            assert_eq!(cid.codec, DAG_CBOR);
            blocks += 1;
            let node = decode(&bytes).expect("strict DAG-CBOR");
            let mut again = Vec::new();
            encode(&node, &mut again).expect("encodes");
            assert_eq!(again, bytes, "{name} encodes to the same bytes");
            let expected = decoded.iter().find(|entry| entry["file"] == name.as_str());
            assert_eq!(
                Some(&json(&node)),
                expected.map(|entry| &entry["value"]),
                "{name}"
            );
        }
    }
    assert_eq!((raw, blocks), (2, 7));
}

const COMPLETION: &str = "bafyreietl3zbyd6nfvyf7aozwwnhacsfod7qhzxusqh2wstwptnvvjn6z4";
const COLLECTION: &str = "bafyreicnx62le63oc5syovippyzgkmaexqa36kdlryjqjzal47rprng3cm";
const ITEM_B: &str = "bafyreieyev4dwko4hmnsjh3s3nauggdnmgdc3sqtabapo23qnxq4ic4qhm";
const BAM_A: &str = "bafkreif7gwanofb74dnqxnb5667ubt6si4johxp6om37lwtpzqslahruwm";
const BAM_B: &str = "bafkreifsfjlickyg7pec4omfkjcxteycwag6af4clpsuh2ur2jrmja567m";

/// The writable store `mine` in `folder`, then the recorded store `lab`, which is only read.
fn stores(folder: &Path) -> storage::Storage {
    let text = format!(
        "[storage.cas]\ntype = \"cas\"\nprefixes = [\"cas://\"]\nwritable = \"mine\"\n\
         resolve = [\"mine\", \"lab\"]\n[storage.cas.stores]\nmine = \"{}\"\nlab = \"{}\"\n",
        folder.display(),
        recorded().display()
    );
    let config = config_file::ConfigFile::parse(&text, Path::new("/test/config.toml"));
    storage::Storage::from_config(&config.expect("TOML")).expect("valid settings")
}

#[tokio::test]
async fn reads_store_addresses() {
    let folder = tempfile::tempdir().expect("a folder");
    let storage = stores(folder.path());
    let downloads = folder.path().join("downloads");
    for address in [
        format!("cas://{BAM_B}/B.bam"),
        format!("cas://{COLLECTION}/{ITEM_B}/B.bam"),
        format!("cas://{ITEM_B}/B.bam"),
        format!("cas://{COMPLETION}/B/B.bam"),
        "cas://lab/B/B.bam".to_owned(),
    ] {
        let found = storage
            .stat(&address)
            .await
            .expect("a stat")
            .expect(&address);
        assert_eq!(
            (found.size, found.version.as_deref()),
            (6, Some(BAM_B)),
            "{address}"
        );
        let copy = storage.fetch(&address, &downloads).await.expect("a copy");
        assert_eq!(std::fs::read_to_string(copy).expect("text"), "bam-B\n");
    }
    let mut folders = storage.list("cas://lab/").await.expect("a listing");
    folders.sort_by(|left, right| left.name.cmp(&right.name));
    let names: Vec<(&str, bool)> = (folders.iter())
        .map(|entry| (entry.name.as_str(), entry.directory))
        .collect();
    assert_eq!(names, [("A", true), ("B", true)]);
    let missing = storage.stat(&format!("cas://{COLLECTION}/C/C.bam")).await;
    assert_eq!(missing.expect("a stat"), None);
}

#[tokio::test]
async fn publishes_content_blocks() {
    let folder = tempfile::tempdir().expect("a folder");
    let store = folder.path().join("mine");
    let storage = stores(&store);
    let file = folder.path().join("A.bam");
    std::fs::write(&file, "bam-A\n").expect("a file");
    let receipt = storage
        .upload_with_receipt(&file, "cas://mine/./A//A.bam")
        .await;
    assert_eq!(receipt.expect("a publish").version.as_deref(), Some(BAM_A));
    let block = store.join(format!("blocks/wm/{BAM_A}"));
    assert_eq!(std::fs::read(&block).expect("the block"), b"bam-A\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&block)
            .expect("the block")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o444);
    }
    let coordinate = std::fs::read_to_string(store.join("coords/A/A.bam")).expect("a coordinate");
    let recorded = std::fs::read_to_string(recorded().join("coords/A/A.bam")).expect("recorded");
    assert_eq!(coordinate, recorded);
    storage
        .upload(&file, "cas://mine/A/A.bam")
        .await
        .expect("the same block again");
    let refused = storage
        .upload(&file, "cas://lab/A/A.bam")
        .await
        .expect_err("read-only");
    assert!(
        refused.to_string().contains("not the writable store"),
        "{refused}"
    );
}

#[test]
fn writes_run_blocks() {
    let folder = tempfile::tempdir().expect("a folder");
    let mine = folder.path().to_string_lossy().into_owned();
    let lab = recorded().to_string_lossy().into_owned();
    storage::cas::use_scope(storage::cas::Options {
        stores: vec![("mine".into(), mine), ("lab".into(), lab)],
        resolve: Vec::new(),
        writable: Some("mine".into()),
    });
    let completion = Cid::parse(COMPLETION).expect("a CID");
    assert_eq!(storage::cas::runs().expect("the Store Log"), [completion]);
    let node = storage::cas::block(&completion)
        .expect("a read")
        .expect("the block");
    assert_eq!(node.get("status").and_then(Node::text), Some("succeeded"));
    let empty = storage::cas::put_block(&Node::Map(Vec::new())).expect("a block");
    let path = folder.path().join(format!("blocks/ua/{empty}"));
    assert_eq!(std::fs::read(path).expect("the block"), [0xa0]);
    storage::cas::put_log("run", &empty).expect("a log entry");
    assert_eq!(
        storage::cas::runs().expect("the Store Log")[0],
        empty,
        "newest first"
    );
    let found = storage::cas::coordinate("cas://lab/A/A.bam").expect("a read");
    let found = found.map(|(cid, size)| (cid.to_string(), size));
    assert_eq!(found, Some((BAM_A.to_owned(), 6)));
}
