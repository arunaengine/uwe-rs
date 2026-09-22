//! Tests for the Sprig functions.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::{base64_decode, base64_encode, call, path_base, path_clean, path_dir, path_ext};
use crate::value::Value;

fn text(value: &str) -> Value {
    Value::string(value)
}

#[test]
fn paths_follow_go() {
    assert_eq!(path_base("/a/b/c.txt"), "c.txt");
    assert_eq!(path_base("/a/b/"), "b");
    assert_eq!(path_base(""), ".");
    assert_eq!(path_dir("/a/b/c.txt"), "/a/b");
    assert_eq!(path_dir("c.txt"), ".");
    assert_eq!(path_ext("s3://b/k/x.tar.gz"), ".gz");
    assert_eq!(path_clean("a//b/../c/."), "a/c");
    assert_eq!(path_clean("/../x"), "/x");
}

#[test]
fn conversions_are_lenient() {
    assert_eq!(call("int", vec![text("12")]), Ok(Value::Int(12)));
    assert_eq!(call("int", vec![text("x")]), Ok(Value::Int(0)));
    assert_eq!(call("int", vec![text("3.0")]), Ok(Value::Int(3)));
    assert_eq!(
        call("min", vec![Value::Int(20), Value::Int(16)]),
        Ok(Value::Int(16))
    );
    assert_eq!(
        call("add", vec![text("2"), Value::Float(3.9)]),
        Ok(Value::Int(5))
    );
}

#[test]
fn repeated_strings_report_unrepresentable_sizes() {
    for subject in ["a", "abc"] {
        assert!(call("repeat", vec![Value::Int(i64::MAX), text(subject)]).is_err());
    }
    assert_eq!(
        call("repeat", vec![Value::Int(i64::MAX), text("")]),
        Ok(text(""))
    );
    assert_eq!(
        call("repeat", vec![Value::Int(3), text("ab")]),
        Ok(text("ababab"))
    );
}

#[test]
fn strings_and_lists() {
    assert_eq!(
        call("trimSuffix", vec![text(".bed"), text("peaks.bed")]),
        Ok(text("peaks"))
    );
    assert_eq!(call("default", vec![text("."), text("")]), Ok(text(".")));
    assert_eq!(call("empty", vec![text("")]), Ok(Value::Bool(true)));
    assert_eq!(
        call(
            "last",
            vec![call("splitList", vec![text("_"), text("a_b_c")]).expect("a list")]
        ),
        Ok(text("c"))
    );
    assert_eq!(
        call(
            "join",
            vec![text(" "), Value::Array(vec![text("a"), Value::Int(1)])]
        ),
        Ok(text("a 1"))
    );
    assert_eq!(
        call("replace", vec![text("-"), text("_"), text("a-b")]),
        Ok(text("a_b"))
    );
    assert!(call("nothing", Vec::new()).is_err());
}

#[test]
fn base64_round_trips() {
    for sample in ["", "a", "ab", "abc", "hello world"] {
        let encoded = base64_encode(sample.as_bytes());
        assert_eq!(base64_decode(&encoded).expect("base64"), sample.as_bytes());
    }
    assert_eq!(base64_encode(b"hi"), "aGk=");
}
