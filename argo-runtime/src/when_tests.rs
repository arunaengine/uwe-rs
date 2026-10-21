//! Tests for `when` conditions, with conditions from the Argo test workflows.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::should_execute;

fn holds(when: &str) -> bool {
    should_execute(when).unwrap_or_else(|error| panic!("{when}: {error}"))
}

#[test]
fn parses_bare_strings() {
    assert!(holds("heads == heads"));
    assert!(!holds("tails == heads"));
    assert!(holds("Succeeded != Failed"));
    assert!(!holds("True == False"));
    assert!(holds("'fastq' == 'fastq'"));
    assert!(holds("\"x\" != \".\""));
}

#[test]
fn evaluates_boolean_logic() {
    assert!(holds("5 > 3"));
    assert!(!holds("3 != 3 && 1 < 2"));
    assert!(holds("false == false"));
    assert!(holds("(1 == 1) && (a == a || b == c)"));
    assert!(holds("heads =~ hea"));
    assert!(holds("3 in (1, 2, 3)"));
    assert!(holds(""));
    assert!(holds("!false"));
}

#[test]
fn missing_compares_false() {
    assert!(!holds(" == heads"));
    assert!(!holds(" != tails"));
    assert!(!holds(" < 10"));
    assert!(!holds(" != 1 && != 2"));
    assert!(holds("'' == ''"));
}

#[test]
fn errors_like_govaluate() {
    assert!(should_execute("true == ").is_ok());
    assert!(should_execute("heads").is_err());
    assert!(should_execute("a.b == c").is_err());
    assert!(should_execute("x == {{inputs.parameters.y}}").is_err());
    assert!(should_execute("1 < a").is_err());
}
