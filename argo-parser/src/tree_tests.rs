//! Tests for building value trees from YAML and JSON text.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use saphyr_parser::{ScalarStyle, Tag};

use super::{parse_at, parse_documents, parse_yaml, resolve};
use crate::node::{Node, Position, Value};

fn start() -> Position {
    Position { line: 1, column: 1 }
}

fn document(text: &str) -> Node {
    parse_at(text, start())
        .expect("valid YAML")
        .expect("one document")
}

fn keys(node: &Node) -> Vec<&str> {
    match &node.value {
        Value::Mapping(entries) => entries.iter().map(|entry| entry.key.as_str()).collect(),
        _ => panic!("expected a mapping"),
    }
}

#[test]
fn records_positions() {
    let node = document("a:\n  b: [1, x]\n");
    let inner = node.get("a").expect("a");
    assert_eq!(inner.position, Position { line: 2, column: 3 });
    let Value::Sequence(items) = &inner.get("b").expect("b").value else {
        panic!("expected a sequence");
    };
    assert_eq!(
        items[1].position,
        Position {
            line: 2,
            column: 10
        }
    );
    assert_eq!(items[1].value, Value::String("x".to_owned()));
}

#[test]
fn shifts_by_origin() {
    let origin = Position {
        line: 10,
        column: 4,
    };
    let node = parse_at(" # note\nx: 1\n", origin)
        .expect("valid")
        .expect("document");
    assert_eq!(
        node.get("x").expect("x").position,
        Position {
            line: 11,
            column: 4
        }
    );
    let flow = parse_at(" {a: 1}", origin)
        .expect("valid")
        .expect("document");
    assert_eq!(
        flow.get("a").expect("a").position,
        Position {
            line: 10,
            column: 9
        }
    );
}

#[test]
fn keys_become_strings() {
    let node = document("1: a\non: b\n'q': c\n");
    assert_eq!(keys(&node), ["1", "true", "q"]);
    // `yes` and `true` are both the boolean true, so they are the same key.
    assert!(parse_at("true: a\nyes: b\n", start()).is_err());
}

#[test]
fn rejects_duplicate_keys() {
    let failure = parse_at("a: 1\nb: 2\na: 3\n", start()).expect_err("duplicate");
    assert_eq!(failure.position, Position { line: 3, column: 1 });
    assert!(
        failure.message.contains("first set on line 1"),
        "{}",
        failure.message
    );
}

#[test]
fn expands_aliases() {
    let node = document("base: &b {a: 1, b: 2}\ncopy: *b\nmerged:\n  <<: *b\n  c: 3\n");
    let value = |key| node.get(key).map(|node| &node.value);
    assert_eq!(value("copy"), value("base"));
    assert_eq!(keys(node.get("merged").expect("merged")), ["a", "b", "c"]);
}

#[test]
fn explicit_keys_win_over_merged_keys() {
    let value = |text: &str| {
        let node = document(text);
        node.get("merged")
            .and_then(|merged| merged.get("x"))
            .map(|x| x.value.clone())
    };
    let base = "base: &b {x: 1, y: 3}\n";
    let before = value(&format!("{base}merged:\n  x: 2\n  <<: *b\n"));
    let after = value(&format!("{base}merged:\n  <<: *b\n  x: 2\n"));
    assert_eq!(before, Some(Value::Integer(2)));
    assert_eq!(after, Some(Value::Integer(2)));
    let first = value("a: &a {x: 1}\nb: &b {x: 5}\nmerged:\n  <<: [*a, *b]\n");
    assert_eq!(first, Some(Value::Integer(1)));
    // Explicit keys still may not repeat.
    assert!(parse_at("m:\n  <<: {x: 1}\n  x: 2\n  x: 3\n", start()).is_err());
}

#[test]
fn quoted_merge_ignored() {
    let node = document("m:\n  '<<': 1\n");
    assert_eq!(keys(node.get("m").expect("m")), ["<<"]);
}

#[test]
fn reads_json() {
    let node = document("{\"a\": [1, 2.5, \"s\", null, true], \"b\": {}}");
    let Value::Sequence(items) = &node.get("a").expect("a").value else {
        panic!("expected a sequence");
    };
    let values: Vec<&Value> = items.iter().map(|item| &item.value).collect();
    assert_eq!(
        values,
        [
            &Value::Integer(1),
            &Value::Float(2.5),
            &Value::String("s".to_owned()),
            &Value::Null,
            &Value::Bool(true)
        ]
    );
}

#[test]
fn reads_first_document() {
    let node = document("a: 1\n...\nb: 2\n");
    assert_eq!(keys(&node), ["a"]);
    assert_eq!(parse_at("# only a comment\n", start()), Ok(None));
}

#[test]
fn reads_all_documents() {
    let documents =
        parse_documents("a: &x 1\n---\n# empty\n---\nb: 2\n...\n---\n- 3\n").expect("valid YAML");
    assert_eq!(documents.len(), 3);
    assert_eq!(keys(&documents[1]), ["b"]);
    assert_eq!(documents[1].position, Position { line: 5, column: 1 });
    // Anchors belong to their own document.
    assert!(parse_documents("a: &x 1\n---\nb: *x\n").is_err());
}

#[test]
fn reads_quoted_scalars_indented_like_their_key() {
    let text = "a:\n  args: [\"\n  echo {{x}};\n  echo \\\"done\\\"\n  \"]\n  b: 'x\n  y'\n";
    let node = parse_yaml(text)
        .expect("go-yaml reads it")
        .expect("a document");
    let a = node.get("a").expect("a");
    let Value::Sequence(args) = &a.get("args").expect("args").value else {
        panic!("a list");
    };
    assert_eq!(
        args[0].value,
        Value::String(" echo {{x}}; echo \"done\" ".to_owned())
    );
    assert_eq!(
        a.get("b").map(|b| &b.value),
        Some(&Value::String("x y".to_owned()))
    );
    let closed = parse_yaml("a:\n  args: [\"\n  x\n  \"\n  ]\n  b: 1\n")
        .expect("go-yaml reads it")
        .expect("a document");
    assert!(closed.get("a").and_then(|a| a.get("b")).is_some());
}

#[test]
fn reports_syntax_errors() {
    let failure = parse_at("a: [1, 2\nb: 3\n", start()).expect_err("unclosed");
    assert!(failure.position.line >= 1);
    assert!(!failure.message.is_empty());
}

#[test]
fn rejects_unknown_aliases() {
    let failure = parse_at("a: *nothing\n", start()).expect_err("unknown");
    assert!(!failure.message.is_empty());
}

fn text(value: &str) -> Value {
    Value::String(value.to_owned())
}

#[test]
fn reads_tagged_scalars() {
    let node = document("a: !!str 12\nb: !!int '7'\nc: !local 3\n");
    assert_eq!(node.get("a").map(|node| &node.value), Some(&text("12")));
    assert_eq!(
        node.get("b").map(|node| &node.value),
        Some(&Value::Integer(7))
    );
    assert_eq!(node.get("c").map(|node| &node.value), Some(&text("3")));
}

#[test]
fn refuses_deep_nesting() {
    // Compact block sequences nest without the parser's own limit for flow collections.
    let deep = format!("{}x", "- ".repeat(2_000));
    let failure = parse_at(&deep, start()).expect_err("too deep");
    assert!(failure.message.contains("nested"), "{}", failure.message);
}

#[test]
fn quoted_stay_strings() {
    let quoted = resolve("true", ScalarStyle::DoubleQuoted, None);
    assert_eq!(quoted, Ok(text("true")));
    let literal = resolve("12", ScalarStyle::Literal, None);
    assert_eq!(literal, Ok(text("12")));
}

#[test]
fn core_tags_resolve() {
    let core = |suffix: &str| Tag {
        handle: "tag:yaml.org,2002:".to_owned(),
        suffix: suffix.to_owned(),
    };
    let quoted = ScalarStyle::DoubleQuoted;
    assert_eq!(
        resolve("12", quoted, Some(&core("int"))),
        Ok(Value::Integer(12))
    );
    assert_eq!(
        resolve("12", ScalarStyle::Plain, Some(&core("str"))),
        Ok(text("12"))
    );
    assert_eq!(
        resolve("3", ScalarStyle::Plain, Some(&core("float"))),
        Ok(Value::Float(3.0))
    );
    assert!(resolve("abc", ScalarStyle::Plain, Some(&core("int"))).is_err());
    let local = Tag {
        handle: "!".to_owned(),
        suffix: "thing".to_owned(),
    };
    assert_eq!(
        resolve("12", ScalarStyle::Plain, Some(&local)),
        Ok(text("12"))
    );
}
