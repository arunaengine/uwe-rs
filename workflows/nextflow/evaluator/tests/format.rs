//! Checks the text of values against Nextflow 26.04.6 (Groovy spec, sections 6.1 to 6.5).
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_evaluator::number::Number;
use nextflow_evaluator::value::Value;

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

#[test]
fn formats_groovy_collections() {
    let mixed = list(vec![n("1"), s("a"), Value::Null, n("1.50"), n("2d")]);
    assert_eq!(mixed.to_groovy_string(), "[1, a, null, 1.50, 2.0]");
    let map = |entries: Vec<(Value, Value)>| Value::map(entries);
    let nested = list(vec![
        n("1"),
        list(vec![
            n("2"),
            list(vec![n("3"), map(vec![(s("a"), list(vec![n("4")]))])]),
        ]),
    ]);
    assert_eq!(nested.to_interpolated(), "[1, [2, [3, [a:[4]]]]]");
    let meta = map(vec![
        (s("id"), s("S1")),
        (s("single_end"), Value::Boolean(false)),
        (s("n"), n("2")),
    ]);
    assert_eq!(meta.to_interpolated(), "[id:S1, single_end:false, n:2]");
    let mixed_map = map(vec![
        (s("a"), list(vec![n("1"), n("2")])),
        (s("b"), map(vec![(s("c"), s("d e"))])),
        (n("1"), Value::Null),
    ]);
    assert_eq!(mixed_map.to_interpolated(), "[a:[1, 2], b:[c:d e], 1:null]");
    assert_eq!(list(vec![]).to_interpolated(), "[]");
    assert_eq!(map(vec![]).to_interpolated(), "[:]");
    assert_eq!(
        Value::set([n("3"), n("1"), n("2")]).to_interpolated(),
        "[3, 1, 2]"
    );
    let units = list(vec![Value::MemoryUnit(2 << 30), Value::Duration(5_400_000)]);
    assert_eq!(units.to_groovy_string(), "[2 GB, 1h 30m]");
}

#[test]
fn formats_java_maps() {
    let map = Value::map([(s("a"), n("1")), (s("b"), list(vec![s("x")]))]);
    assert_eq!(map.to_java_string(), "{a=1, b=[x]}");
    assert_eq!(list(vec![map.clone()]).to_java_string(), "[{a=1, b=[x]}]");
    assert_eq!(list(vec![map]).to_groovy_string(), "[[a:1, b:[x]]]");
    let entry = Value::MapEntry(Rc::new((s("a"), Value::map([(s("b"), n("1"))]))));
    assert_eq!(entry.to_groovy_string(), "a={b=1}");
    assert_eq!(Value::map([]).to_java_string(), "{}");
}

#[test]
fn self_references() {
    let items = list(vec![n("1")]);
    let Value::List(shared) = &items else {
        unreachable!()
    };
    shared.borrow_mut().items.push(items.clone());
    assert_eq!(items.to_groovy_string(), "[1, (this Collection)]");
    let map = Value::map([]);
    let Value::Map(shared) = &map else {
        unreachable!()
    };
    let key = nextflow_evaluator::value::Key(s("me"));
    shared.borrow_mut().entries.insert(key, map.clone());
    assert_eq!(map.to_groovy_string(), "[me:(this Map)]");
    // Break the cycles so the test does not leak.
    shared.borrow_mut().entries.clear();
    let Value::List(shared) = &items else {
        unreachable!()
    };
    shared.borrow_mut().items.clear();
}

#[test]
fn scalars() {
    assert_eq!(Value::Null.to_groovy_string(), "null");
    assert_eq!(Value::Boolean(true).to_string(), "true");
    assert_eq!(n("1e3").to_string(), "1E+3");
    assert_eq!(Value::GString("x y".into()).to_interpolated(), "x y");
    assert_eq!(Value::MemoryUnit(0).to_string(), "0");
}
