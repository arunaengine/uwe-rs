//! Checks regular expressions against Java's `String` methods (Groovy spec, section 8.6).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_evaluator::regex::Pattern;

fn pattern(source: &str) -> Pattern {
    Pattern::new(source).expect("valid pattern")
}

#[test]
fn find_and_match() {
    assert!(pattern("b").find("abc"));
    assert!(!pattern("b").matches("abc"));
    assert!(pattern("a.c").matches("abc"));
    assert!(pattern("(?i)abc").matches("ABC"));
    assert!(pattern(r"\d+").matches("123"));
    assert!(!pattern("a|b").matches("ab"));
    assert!(Pattern::new("(").is_err());
}

#[test]
fn split_like_java() {
    let split = |text: &str, regex: &str, limit| pattern(regex).split(text, limit);
    assert_eq!(split("x_y_z", "_", 0), ["x", "y", "z"]);
    assert_eq!(split("a.b.c", ".", 0), Vec::<String>::new());
    assert_eq!(split("a,b,,", ",", 0), ["a", "b"]);
    assert_eq!(split("a,b,,", ",", -1), ["a", "b", "", ""]);
    assert_eq!(split("", ",", 0), [""]);
    assert_eq!(split(",a", ",", 0), ["", "a"]);
    assert_eq!(split("abc", "", 0), ["a", "b", "c"]);
    assert_eq!(split("a,b,c", ",", 2), ["a", "b,c"]);
    assert_eq!(split("a1b22c", r"\d+", 0), ["a", "b", "c"]);
}

#[test]
fn replace_like_java() {
    let replace = |text: &str, regex: &str, with: &str| pattern(regex).replace(text, with, true);
    assert_eq!(
        replace("sample_R1.fq", r"_R(\d)", "-$1").unwrap(),
        "sample-1.fq"
    );
    assert_eq!(replace("aaa", "a", "b").unwrap(), "bbb");
    assert_eq!(replace("a.b", r"\.", r"\$").unwrap(), "a$b");
    assert_eq!(replace("ab", "(?<x>a)", "${x}${x}").unwrap(), "aab");
    assert_eq!(replace("ab", "(a)", "$12").unwrap(), "a2b");
    assert_eq!(pattern("a").replace("aaa", "b", false).unwrap(), "baa");
    assert!(replace("ab", "a", "$").is_err());
    assert!(replace("ab", "a", "$2").is_err());
}
