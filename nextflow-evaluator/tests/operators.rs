//! Checks operators on values against Nextflow 26.04.6 (Groovy spec, sections 5.6, 8.5 and 8.13).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::rc::Rc;

use nextflow_evaluator::number::Number;
use nextflow_evaluator::value::{List, ListKind, Operator, Value};

fn n(text: &str) -> Value {
    Value::Number(
        Number::parse_integer(text)
            .or_else(|_| Number::parse_decimal(text))
            .expect("valid literal"),
    )
}

fn s(text: &str) -> Value {
    Value::string(text)
}

fn list(items: Vec<Value>) -> Value {
    Value::list(items)
}

fn apply(left: &Value, operator: Operator, right: &Value) -> String {
    left.apply(operator, right)
        .expect("operator applies")
        .to_groovy_string()
}

fn error(left: &Value, operator: Operator, right: &Value) -> String {
    left.apply(operator, right)
        .expect_err("operator fails")
        .class
        .to_owned()
}

#[test]
fn strings() {
    use Operator::*;
    assert_eq!(apply(&s("x"), Add, &list(vec![n("1"), n("2")])), "x[1, 2]");
    assert_eq!(apply(&s("x"), Add, &Value::Null), "xnull");
    assert_eq!(apply(&n("1"), Add, &s("x")), "1x");
    assert_eq!(apply(&Value::Null, Add, &s("x")), "nullx");
    let three = n("1").apply(Add, &n("2")).unwrap();
    assert_eq!(apply(&three, Add, &s("a")), "3a");
    assert_eq!(apply(&s("a-b-c"), Subtract, &s("-")), "ab-c");
    assert_eq!(apply(&s("ab"), Multiply, &n("3")), "ababab");
    assert_eq!(apply(&s("a"), Multiply, &n("0")), "");
    assert_eq!(
        error(&s("a"), Multiply, &n("-1")),
        "IllegalArgumentException"
    );
    let failure = Value::Null.apply(Add, &n("1")).unwrap_err();
    assert_eq!(failure.message, "Cannot execute null+1");
}

#[test]
fn lists_and_sets() {
    use Operator::*;
    assert_eq!(apply(&list(vec![n("10")]), Add, &n("40")), "[10, 40]");
    assert_eq!(
        apply(&list(vec![n("10")]), Add, &list(vec![n("40")])),
        "[10, 40]"
    );
    let nested = list(vec![list(vec![n("40")])]);
    assert_eq!(apply(&list(vec![n("10")]), Add, &nested), "[10, [40]]");
    let numbers = list(vec![n("1"), n("2"), n("2"), n("3")]);
    assert_eq!(apply(&numbers, Subtract, &list(vec![n("2")])), "[1, 3]");
    assert_eq!(
        apply(&list(vec![n("1"), n("2")]), Subtract, &list(vec![n("2L")])),
        "[1]"
    );
    assert_eq!(
        apply(&list(vec![n("1.0")]), Subtract, &list(vec![n("1.00")])),
        "[]"
    );
    let mixed = list(vec![n("1"), n("2"), n("2L"), n("3")]);
    assert_eq!(apply(&mixed, Subtract, &n("2")), "[1, 3]");
    assert_eq!(
        apply(&list(vec![n("10"), n("20")]), Multiply, &n("2")),
        "[10, 20, 10, 20]"
    );
    assert_eq!(apply(&list(vec![n("1")]), Multiply, &n("0")), "[]");

    let target = list(vec![n("1")]);
    let result = target.apply(LeftShift, &n("2")).unwrap();
    assert!(matches!((&target, &result), (Value::List(a), Value::List(b)) if Rc::ptr_eq(a, b)));
    assert_eq!(target.to_string(), "[1, 2]");

    let tuple = Value::List(Rc::new(RefCell::new(List::new(
        ListKind::Tuple,
        vec![n("1")],
    ))));
    assert_eq!(
        error(&tuple, Add, &list(vec![n("2")])),
        "UnsupportedOperationException"
    );
    assert_eq!(
        error(&tuple, LeftShift, &n("2")),
        "UnsupportedOperationException"
    );

    let set = Value::set([n("1"), n("2")]);
    assert_eq!(apply(&set, Add, &list(vec![n("2"), n("3")])), "[1, 2, 3]");
    assert_eq!(apply(&set, Subtract, &list(vec![n("1")])), "[2]");
    assert_eq!(set.to_string(), "[1, 2]");
    assert_eq!(
        list(vec![n("1"), n("2")]).negate().unwrap().to_string(),
        "[-1, -2]"
    );
}

#[test]
fn maps() {
    use Operator::*;
    let map = |entries: Vec<(&str, Value)>| {
        Value::map(entries.into_iter().map(|(key, value)| (s(key), value)))
    };
    let merged = map(vec![("b", n("2"))])
        .apply(Add, &map(vec![("a", n("1"))]))
        .unwrap();
    assert_eq!(apply(&merged, Add, &map(vec![("b", n("3"))])), "[b:3, a:1]");
    let both = map(vec![("a", n("1")), ("b", n("2"))]);
    assert_eq!(apply(&both, Subtract, &map(vec![("a", n("1"))])), "[b:2]");
    assert_eq!(
        apply(&both, Subtract, &map(vec![("a", n("5"))])),
        "[a:1, b:2]"
    );
    both.apply(LeftShift, &map(vec![("c", n("3"))])).unwrap();
    assert_eq!(both.to_string(), "[a:1, b:2, c:3]");
}

#[test]
fn units_and_booleans() {
    use Operator::*;
    let gb = Value::MemoryUnit(1 << 30);
    let hour = Value::Duration(3_600_000);
    assert_eq!(
        apply(&Value::MemoryUnit(2 << 30), Multiply, &n("1.5")),
        "3 GB"
    );
    assert_eq!(apply(&n("2"), Multiply, &hour), "2h");
    assert_eq!(apply(&n("1.5d"), Multiply, &gb), "1.5 GB");
    assert_eq!(apply(&gb, Add, &gb), "2 GB");
    assert_eq!(apply(&hour, Divide, &n("7")), "8m 34s");
    assert_eq!(apply(&gb, Add, &Value::Null), "1 GB");
    assert_eq!(error(&gb, Add, &n("1")), "MissingMethodException");
    assert_eq!(
        error(&gb, Subtract, &Value::MemoryUnit(2 << 30)),
        "AssertionError"
    );
    assert_eq!(
        apply(&Value::Boolean(true), Xor, &Value::Boolean(true)),
        "false"
    );
    assert_eq!(
        apply(&Value::Boolean(true), BitOr, &Value::Boolean(false)),
        "true"
    );
}

#[test]
fn membership() {
    let contains = |container: &Value, item: &Value| container.is_case(item).unwrap();
    assert!(!contains(&list(vec![n("1"), n("2")]), &n("2L")));
    assert!(contains(&list(vec![n("1"), n("2")]), &n("2")));
    assert!(!contains(&list(vec![n("1.00")]), &n("1.0")));
    assert!(contains(&list(vec![Value::Null]), &Value::Null));
    let engines = list(vec![s("singularity"), s("apptainer")]);
    assert!(!contains(&engines, &s("docker")));
    assert!(!contains(&Value::map([(s("a"), n("0"))]), &s("a")));
    assert!(!contains(&Value::map([(s("a"), Value::Null)]), &s("a")));
    assert!(contains(&Value::map([(s("a"), n("1"))]), &s("a")));
    assert!(!contains(&s("abc"), &s("b")));
    assert!(contains(&s("1"), &n("1")));
    assert!(contains(&n("1.0"), &n("1")));
    assert!(contains(&Value::Null, &Value::Null));
    assert!(contains(&Value::set([n("1")]), &n("1")));
}
