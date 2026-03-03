//! Checks list and set methods against Nextflow 26.04.6 (Groovy spec, sections 8.12 and 10.2 to 10.4).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_evaluator::interpreter::{Host, Interpreter};
use nextflow_evaluator::lower::lower_module;
use nextflow_parser::parse_script;

struct Silent;

impl Host for Silent {
    fn print(&mut self, _: &str) {}
}

/// The value of the body of a function, as its Groovy text, or the error.
fn eval(body: &str) -> Result<String, String> {
    let source = format!("def f() {{\n{body}\n}}");
    let parsed = parse_script(&source);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let module = Rc::new(lower_module(&parsed.tree, &source).expect("lowers"));
    let mut host = Silent;
    let mut interpreter = Interpreter::new(&mut host);
    interpreter
        .call_function(&module, "f", Vec::new())
        .map(|value| value.to_string())
        .map_err(|error| error.to_string())
}

fn value(body: &str) -> String {
    eval(body).expect("evaluates")
}

#[test]
fn queries_and_folds() {
    assert_eq!(
        value("[[3, 1, 2].intersect([2, 3]), [1, 1, 2].intersect([1]), [1, 2].intersect([1L])]"),
        "[[3, 2], [1, 1], [1]]"
    );
    assert_eq!(
        value("[[1, 2, 3].sum(), [].sum(), [[1], [2]].sum(), ['a', 'b'].sum()]"),
        "[6, null, [1, 2], ab]"
    );
    assert_eq!(value("[1, 2].inject(10) { a, b -> a + b }"), "13");
    assert_eq!(
        value("[[null, 3, 1].max(), [null, 3, 1].min(), [1, 'a'].max()]"),
        "[3, 1, a]"
    );
    assert_eq!(
        value("[['x', 'y'].indexOf('y'), [1, 2, 3].count { it > 1 }, [1, 2].contains(2L)]"),
        "[1, 2, false]"
    );
    assert_eq!(value("[0, 1, '', 'a', null, []].findAll()"), "[1, a]");
    assert_eq!(
        value("[[1, 2].join(), [1, null, 'a', 1.50, [2, 3]].join(',')]"),
        "[12, 1,null,a,1.50,[2, 3]]"
    );
    assert_eq!(
        value("[[1, 2].any { it > 1 }, [1, 2].every { it > 1 }, [].every { false }]"),
        "[true, false, true]"
    );
    assert_eq!(value("[1, 2, 3].find { it > 1 }"), "2");
    assert_eq!(
        value("def total = 0\n[1, 2].each { total += it }\ntotal"),
        "3"
    );
}

#[test]
fn transformations() {
    assert_eq!(value("[1, [2, [3]]].flatten()"), "[1, 2, 3]");
    assert_eq!(value("[4, 5].withIndex()"), "[[4, 0], [5, 1]]");
    assert_eq!(
        value("[[1, 'a'], [2, 'b']].transpose()"),
        "[[1, 2], [a, b]]"
    );
    assert_eq!(
        value("[[1, 2, 3].collate(2), [1, 2, 3, 4].collate(3, 1, false)]"),
        "[[[1, 2], [3]], [[1, 2, 3], [2, 3, 4]]]"
    );
    assert_eq!(
        value("[[1, 2, 3].take(2), [1, 2, 3].drop(1), [1, 2, 3].reverse()]"),
        "[[1, 2], [2, 3], [3, 2, 1]]"
    );
    assert_eq!(value("[1, 2, 3].groupBy { it % 2 }"), "[1:[1, 3], 0:[2]]");
    assert_eq!(
        value("['aa', 'b'].collectEntries { [it, it.size()] }"),
        "[aa:2, b:1]"
    );
    assert_eq!(value("[1, 2].collect { it * 2 }"), "[2, 4]");
    assert_eq!(
        value("[[1, 2], [3, 4]].collect { a, b -> a + b }"),
        "[3, 7]"
    );
    assert_eq!(value("[1, 2].collectMany { [it, it] }"), "[1, 1, 2, 2]");
}

#[test]
fn sorting_and_duplicates() {
    assert_eq!(value("def l = [3, 1, 2]\nl.sort()\nl"), "[1, 2, 3]");
    assert_eq!(
        value("['b', 'A', 'a', 'B', '10', '9'].toSorted()"),
        "[10, 9, A, B, a, b]"
    );
    assert_eq!(
        value("[[2, null, 1].toSorted(), [3, 1.5, 2L, 0.5d].toSorted()]"),
        "[[null, 1, 2], [0.5, 1.5, 2, 3]]"
    );
    assert_eq!(value("['bb', 'a'].sort { it.size() }"), "[a, bb]");
    assert_eq!(value("[1, 3, 2].sort { a, b -> b <=> a }"), "[3, 2, 1]");
    assert_eq!(
        value("[[1, 1L, 1.0, 2].unique(), ['b', 'a', 'b'].toUnique()]"),
        "[[1, 2], [b, a]]"
    );
    assert_eq!(
        value("[3, 1, 3, 2, 10, 100, -1].toSet()"),
        "[-1, 1, 2, 10, 3, 100]"
    );
    assert_eq!(
        value("['sample', 'b', 'a', 'zz', 'aa'].toSet()"),
        "[zz, aa, a, b, sample]"
    );
    assert_eq!(value("def t = tuple(3, 1, 2)\nt.sort()\nt"), "[1, 2, 3]");
}

#[test]
fn positions_and_changes() {
    assert_eq!(
        eval("[].first()").unwrap_err(),
        "NoSuchElementException: Cannot access first() element from an empty List"
    );
    assert_eq!(
        value("[[1, 2].first(), [1, 2].last(), [1, 2, 3].tail(), [1, 2].get(1)]"),
        "[1, 2, [2, 3], 2]"
    );
    assert!(
        eval("[1, 2].get(5)")
            .unwrap_err()
            .starts_with("IndexOutOfBoundsException")
    );
    assert_eq!(
        value("def l = ['a', 'b']\nl.add('c')\nl.remove(0)\nl.remove('c')\nl"),
        "[b]"
    );
    assert_eq!(
        value("def l = [1, 2, 3]\nl.removeIf { it > 1 }\nl.addAll([7, 8])\nl"),
        "[1, 7, 8]"
    );
    assert_eq!(
        eval("tuple(1, 2).add(3)").unwrap_err(),
        "UnsupportedOperationException: "
    );
    assert_eq!(
        value("def s = [1, 2] as Set\ns.add(2)\ns.add(3)\ns.remove(1)\ns"),
        "[2, 3]"
    );
}

#[test]
fn list_properties() {
    assert_eq!(
        value("[['ab', 'c'].size, [].empty, [1, 2].first, [1, 2].last]"),
        "[2, true, 1, 2]"
    );
    assert_eq!(value("[[a: 1], [a: 2], [b: 3], null].a"), "[1, 2, null]");
}
