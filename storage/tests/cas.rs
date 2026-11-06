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
