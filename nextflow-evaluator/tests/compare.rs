//! Checks `==`, `<` and `<=>` against Nextflow 26.04.6 (Groovy spec, sections 8.2 to 8.4).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::rc::Rc;

use nextflow_evaluator::number::Number;
use nextflow_evaluator::value::{List, ListKind, Relation, Value};

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

fn equal(left: &Value, right: &Value) -> bool {
    left.compare_equal(right).expect("comparable")
}

fn less(left: &Value, right: &Value) -> Result<bool, String> {
    left.compare_relation(Relation::Less, right)
        .map_err(|error| error.class.to_owned())
}

fn kind_list(kind: ListKind, items: Vec<Value>) -> Value {
    Value::List(Rc::new(RefCell::new(List { kind, items })))
}

#[test]
fn numbers_compare_by_value() {
    assert!(equal(&n("1"), &n("1L")));
    assert!(equal(&n("1"), &n("1.0")));
    assert!(equal(&n("1.0"), &n("1.00")));
    assert!(equal(&n("0.1"), &n("0.1d")));
    assert!(!equal(&n("0.1f"), &n("0.1d")));
    assert!(!equal(&n("0.0d"), &n("-0.0d")));
    let nan = Value::Number(Number::Double(f64::NAN));
    assert!(equal(&nan, &nan));
    assert_eq!(nan.compare_relation(Relation::Greater, &n("1d")), Ok(false));
    assert_eq!(nan.compare_relation(Relation::Greater, &n("1")), Ok(true));
}

#[test]
fn mixed_classes() {
    assert!(equal(&n("97"), &s("a")));
    assert!(equal(&s("a"), &n("97")));
    assert!(!equal(&n("1"), &s("1")));
    assert!(!equal(&s("abc"), &n("5")));
    assert!(!equal(&n("5"), &s("abc")));
    assert!(!equal(&Value::Boolean(true), &s("true")));
    assert!(!equal(&Value::list(vec![n("1")]), &s("[1]")));
    assert!(equal(&s("a"), &Value::GString("a".into())));
    assert!(equal(&Value::Null, &Value::Null));
    assert!(!equal(&Value::Null, &n("0")));
}

#[test]
fn collections_compare_their_elements() {
    let list = |items: Vec<Value>| Value::list(items);
    assert!(equal(
        &list(vec![n("1"), n("2")]),
        &list(vec![n("1L"), n("2L")])
    ));
    assert!(equal(
        &list(vec![n("1"), n("2")]),
        &list(vec![n("1.0"), n("2.0")])
    ));
    let tuple = kind_list(ListKind::Tuple, vec![s("a"), n("1")]);
    assert!(equal(&tuple, &list(vec![s("a"), n("1")])));
    let array = kind_list(ListKind::Array, vec![s("a"), s("b")]);
    assert!(equal(&list(vec![s("a"), s("b")]), &array));
    assert!(!equal(&Value::set([n("1")]), &list(vec![n("1")])));
    assert!(equal(
        &Value::set([n("1"), n("2")]),
        &Value::set([n("2L"), n("1L")])
    ));
    let map = |key: Value| Value::map([(key, s("a"))]);
    assert!(!equal(&map(n("1")), &map(n("1L"))));
    let left = Value::map([(s("a"), n("1")), (s("b"), n("2"))]);
    let right = Value::map([(s("b"), n("2.0")), (s("a"), n("1L"))]);
    assert!(equal(&left, &right));
}

#[test]
fn units_follow_nextflow() {
    let gb = Value::MemoryUnit(1 << 30);
    let zero = Value::MemoryUnit(0);
    assert!(equal(&gb, &s("1 GB")));
    assert!(equal(&gb, &n("1073741824")));
    assert!(equal(&n("1073741824"), &gb));
    assert!(!equal(&Value::Null, &gb));
    let error = zero.compare_equal(&zero).unwrap_err();
    assert_eq!(error.class, "AssertionError");
    assert_eq!(gb.compare_relation(Relation::Greater, &zero), Ok(true));
    assert_eq!(less(&zero, &gb), Err("AssertionError".to_owned()));
    assert_eq!(less(&gb, &Value::Null), Ok(false));
    assert_eq!(less(&Value::Duration(1000), &s("2s")), Ok(true));
    // Inside a list, Groovy compares units with their own `compareTo`, without the assert.
    let zeros = Value::list(vec![zero.clone()]);
    assert!(equal(&zeros, &Value::list(vec![Value::MemoryUnit(0)])));
}

#[test]
fn ordering() {
    assert_eq!(less(&Value::Null, &n("1")), Ok(true));
    assert_eq!(less(&n("1"), &Value::Null), Ok(false));
    assert_eq!(less(&s("10"), &s("9")), Ok(true));
    assert_eq!(less(&s("B"), &s("a")), Ok(true));
    assert_eq!(less(&n("1"), &s("2")), Ok(true));
    let one = Value::list(vec![n("1")]);
    assert_eq!(
        less(&one, &Value::list(vec![n("2")])),
        Err("IllegalArgumentException".into())
    );
    let error = n("1").groovy_compare(&s("ab")).unwrap_err();
    assert_eq!(
        error.message,
        "Cannot compare java.lang.Integer with value '1' and java.lang.String with value 'ab'"
    );
    // A character above U+FFFF sorts by its UTF-16 surrogates, below U+FFFD.
    assert_eq!(less(&s("\u{1F600}"), &s("\u{FFFD}")), Ok(true));
}

#[test]
fn spaceship() {
    let order = |left: &Value, right: &Value| left.spaceship(right).map(|value| value.to_string());
    assert_eq!(order(&s("a"), &s("c")), Ok("-1".to_owned()));
    assert_eq!(order(&n("2"), &n("1.5")), Ok("1".to_owned()));
    assert_eq!(
        order(&Value::MemoryUnit(0), &Value::MemoryUnit(2 << 30)),
        Ok("-1".into())
    );
    let error = Value::MemoryUnit(1 << 30)
        .spaceship(&s("1 GB"))
        .unwrap_err();
    assert_eq!(error.class, "IllegalArgumentException");
}
