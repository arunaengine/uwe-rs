//! Checks Groovy truth, class names and Java equality of values (Groovy spec, sections 3, 7 and 8.11).
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_evaluator::number::Number;
use nextflow_evaluator::value::{Key, Value};

fn n(text: &str) -> Value {
    Value::Number(
        Number::parse_integer(text)
            .or_else(|_| Number::parse_decimal(text))
            .expect("valid literal"),
    )
}

#[test]
fn groovy_truth() {
    let falsy = [
        Value::Null,
        Value::Boolean(false),
        n("0"),
        n("0L"),
        n("0.0"),
        n("1e-400"),
        n("0d"),
        Value::Number(Number::Double(f64::NAN)),
        Value::string(""),
        Value::list(vec![]),
        Value::map([]),
        Value::set([]),
        Value::MemoryUnit(0),
        Value::Duration(0),
    ];
    for value in falsy {
        assert!(!value.truth(), "{value:?}");
    }
    let truthy = [
        Value::Boolean(true),
        n("1"),
        n("-0.5"),
        Value::string(" "),
        Value::string("false"),
        Value::string("0"),
        Value::list(vec![Value::Null]),
        Value::map([(Value::string("a"), Value::Null)]),
        Value::MemoryUnit(1),
    ];
    for value in truthy {
        assert!(value.truth(), "{value:?}");
    }
}

#[test]
fn class_names() {
    assert_eq!(n("1").class_name(), "Integer");
    assert_eq!(n("1.5").class_name(), "BigDecimal");
    assert_eq!(Value::string("a").class_name(), "String");
    assert_eq!(Value::GString("a".into()).class_name(), "GStringImpl");
    assert_eq!(Value::list(vec![]).class_name(), "ArrayList");
    assert_eq!(Value::map([]).class_name(), "LinkedHashMap");
    assert_eq!(Value::set([]).class_name(), "LinkedHashSet");
}

#[test]
fn distinguishes_java_classes() {
    assert!(n("1").java_equals(&n("1")));
    assert!(!n("1").java_equals(&n("1L")));
    assert!(!n("1.0").java_equals(&n("1.00")));
    assert!(!n("0.0d").java_equals(&n("-0.0d")));
    let nan = Value::Number(Number::Double(f64::NAN));
    assert!(nan.java_equals(&Value::Number(Number::Double(-f64::NAN))));
    assert!(!Value::string("a").java_equals(&Value::GString("a".into())));
    let list = |items: Vec<Value>| Value::list(items);
    assert!(list(vec![n("1"), n("2")]).java_equals(&list(vec![n("1"), n("2")])));
    assert!(!list(vec![n("1")]).java_equals(&list(vec![n("1L")])));
    let left = Value::map([(Value::string("a"), n("1")), (Value::string("b"), n("2"))]);
    let right = Value::map([(Value::string("b"), n("2")), (Value::string("a"), n("1"))]);
    assert!(left.java_equals(&right));
    assert!(Value::set([n("1"), n("2")]).java_equals(&Value::set([n("2"), n("1")])));
    assert!(!Value::set([n("1")]).java_equals(&list(vec![n("1")])));
}

#[test]
fn java_hash_codes() {
    assert_eq!(Value::string("abc").java_hash(), 96354);
    assert_eq!(Value::string("").java_hash(), 0);
    assert_eq!(Value::Boolean(true).java_hash(), 1231);
    assert_eq!(n("-5").java_hash(), -5);
    assert_eq!(n("4294967297L").java_hash(), 0);
    assert_eq!(n("1.5d").java_hash(), 1073217536);
    assert_eq!(n("1.5").java_hash(), 466);
    assert_eq!(n("18446744073709551616").java_hash(), 961);
    assert_eq!(n("-18446744073709551616").java_hash(), -961);
    let list = Value::list(vec![n("1"), n("2"), n("3")]);
    assert_eq!(list.java_hash(), 30817);
    let map = Value::map([(Value::string("a"), n("1"))]);
    assert_eq!(map.java_hash(), 96);
    assert_eq!(Value::GString("a".into()).java_hash(), 37 + 97);
}

#[test]
fn compares_java_keys() {
    let map = Value::map([
        (n("1"), Value::string("int")),
        (n("1L"), Value::string("long")),
    ]);
    let Value::Map(map) = map else { unreachable!() };
    let map = map.borrow();
    assert_eq!(map.entries.len(), 2);
    assert!(map.entries.get(&Key(n("1.0"))).is_none());
    let replaced = Value::map([(Value::string("a"), n("1")), (Value::string("a"), n("2"))]);
    let Value::Map(replaced) = replaced else {
        unreachable!()
    };
    let value = replaced
        .borrow()
        .entries
        .get(&Key(Value::string("a")))
        .cloned();
    assert!(value.is_some_and(|value| value.java_equals(&n("2"))));
}
