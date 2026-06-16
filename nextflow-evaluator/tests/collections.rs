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

/// Groovy's `combinations()`, as Nextflow 26.04.6 gives it.
#[test]
fn combinations() {
    assert_eq!(
        value(
            "[[[1, 2], ['a', 'b'], ['x']].combinations(), [[1, 2], 'c'].combinations(), [].combinations()]"
        ),
        "[[[1, a, x], [2, a, x], [1, b, x], [2, b, x]], [[1, c], [2, c]], []]"
    );
}

#[test]
fn index_values_and_lists_of_indexes() {
    assert_eq!(
        value(
            "def files = ['a.txt', 'b.bed', 'c.bed']\n\
             def ind = files.findIndexValues { f -> f.endsWith('.bed') }\n\
             [ind, ind[0].getClass().getSimpleName(), files[ind], [10, 20, 30, 40][[0, -1, 1..2, 7]], \
             [a: 1, b: 2].collectMany { k, v -> [k, v] }]"
        ),
        "[[1, 2], Long, [b.bed, c.bed], [10, 40, 20, 30, null], [a, 1, b, 2]]"
    );
}

#[test]
fn null_iterates_as_nothing() {
    assert_eq!(
        value(
            "def n = null\n\
             [n.findAll { x -> true }, n.collect { x -> x }, n.each { x -> x }, n.find { x -> true }, \
             n.any { x -> true }, n.every { x -> false }, n.grep(), n.inject(5) { a, b -> a }, \
             n.asBoolean()]"
        ),
        "[[], [], null, null, false, true, [], 5, false]"
    );
    assert_eq!(
        eval("null.size()").unwrap_err(),
        "NullPointerException: Cannot invoke method size() on null object"
    );
}

#[test]
fn hash_sets_keep_the_order_of_java_tables() {
    assert_eq!(
        value(
            "def s = new HashSet()\n\
             ['pna-2', 'mpx', 'zeta', 'alpha', 'b'].each { v -> s << v }\n\
             def l = new LinkedHashSet(['z', 'a'])\n\
             l << 'b'\n\
             [s, new HashSet([3, 1, 2, 1]), l, s.getClass().getSimpleName()]"
        ),
        "[[zeta, b, mpx, pna-2, alpha], [1, 2, 3], [z, a, b], HashSet]"
    );
}

#[test]
fn grep_and_sort_without_change() {
    assert_eq!(
        value(
            "[[1, null, 'x', '', 0].grep(), ['ab', 'cd'].grep(~/a.*/), ['ab', null].grep { it != null }]"
        ),
        "[[1, x], [ab], [ab]]"
    );
    assert_eq!(
        value("def l = [3, 1, 2]\n[l.sort(false) { a, b -> a <=> b }, l]"),
        "[[1, 2, 3], [3, 1, 2]]"
    );
    assert_eq!(
        value(
            "[file('/a/b/c.txt').sort(false) { a, b -> b.name <=> a.name }, file('/a/b/c.txt').sort()]"
        ),
        "[[c.txt, b, a], [a, b, c.txt]]"
    );
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
    assert_eq!(value("[4, 5].withIndex(1)"), "[[4, 1], [5, 2]]");
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
    // Groovy 4.0.31 prints these.
    assert_eq!(
        value("def l = [1, 2]\ndef r = l.push(3)\ndef m = l.toString()\n[l.pop(), l, m, r]"),
        "[3, [1, 2], [3, 1, 2], true]"
    );
    assert_eq!(
        eval("[].pop()").unwrap_err(),
        "NoSuchElementException: Cannot pop() an empty List"
    );
    // Lists have no path methods; Groovy 4.0.31 says so.
    assert_eq!(
        eval("[1, 'a'].getExtension()").unwrap_err(),
        "MissingMethodException: No signature of method: java.util.ArrayList.getExtension() is \
         applicable for argument types: () values: []"
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

#[test]
fn more_collection_and_path_methods() {
    assert_eq!(
        value(
            "def m = [a: 1, b: 2, c: 3]\ndef l = [3, 1, 3]\n\
             [m.subMap('a', 'c'), [b: 2, a: 1].toSorted { x, y -> x.key <=> y.key }, (1..10).step(3), \
             file('/a/b/c.txt').endsWith('b/c.txt'), file('/a/b/c.txt').startsWith('/a'), 5.collect { it * 2 }, \
             l.unique(false), l, [3, 1, 2].sort(false)]"
        ),
        "[[a:1, c:3], [a:1, b:2], [1, 4, 7, 10], true, true, [10], [3, 1], [3, 1, 3], [1, 2, 3]]"
    );
}

#[test]
fn collection_classes() {
    assert_eq!(value("[1].getClass() == ArrayList"), "true");
    assert_eq!(value("[a: 1].getClass() == LinkedHashMap"), "true");
    assert_eq!(value("[a: 1].getClass() == ArrayList"), "false");
}

/// A list indexed with a name gives that property of each element that is not null, as
/// Groovy's `getAt(String)` does; Nextflow 26.04.6 prints these.
#[test]
fn lists_indexed_with_names() {
    let list = "def l = [[id: 1, c: 2], null, [id: 3]]";
    assert_eq!(value(&format!("{list}\nl['id']")), "[1, 3]");
    assert_eq!(value(&format!("{list}\nl['c']")), "[2, null]");
    assert_eq!(value("[]['id']"), "[]");
}

/// Several indexes give the element at each, as Groovy's `getAt(Collection)`.
#[test]
fn lists_indexed_with_several_indexes() {
    assert_eq!(value("[10, 11, 12, 13][0, 2, 3]"), "[10, 12, 13]");
    assert_eq!(value("[10, 11, 12, 13][0, 1..2]"), "[10, 11, 12]");
}

/// A method that Groovy does not have for a list fails with Groovy's message, as Nextflow
/// 26.04.6 gives it.
#[test]
fn methods_that_lists_lack() {
    assert_eq!(
        eval("['a.b', 'c.d'].tokenize('.')").unwrap_err(),
        "MissingMethodException: No signature of method: java.util.ArrayList.tokenize() is \
         applicable for argument types: (String) values: [.]"
    );
}

/// Array classes like `Object[]` and `isAssignableFrom`, as the `isCollectionOrArray` of older
/// nf-core pipelines uses them; Nextflow 26.04.6 with the legacy parser gives these.
#[test]
fn array_classes_and_assignable_classes() {
    let check = "def f = { x -> [Collection, Object[]].any { it.isAssignableFrom(x.getClass()) } }";
    assert_eq!(value(&format!("{check}\nf.call([1])")), "true");
    assert_eq!(value(&format!("{check}\nf.call('x')")), "false");
}

/// Legacy syntax: `*items` in a list and `*: map` in a map put their elements in; Nextflow
/// 26.04.6 prints `[1, 2, 3]` and `[a:1, b:2]`.
#[test]
fn spread_elements_and_entries() {
    assert_eq!(
        value("def b = [2, 3]\ndef m = [a: 1]\n[[1, *b], [*:m, b: 2]]"),
        "[[1, 2, 3], [a:1, b:2]]"
    );
}

/// A last parameter of an array type takes the remaining arguments, like `... tail` of the
/// Swedish Earth Biogenome Project; Nextflow 26.04.6 prints `[1, 2, [3, 4]]`.
#[test]
fn varargs_parameters() {
    assert_eq!(
        value("def r = { key, clip, ... tail -> [key, clip, tail.toList()] }\nr(1, 2, 3, 4)"),
        "[1, 2, [3, 4]]"
    );
    assert_eq!(
        value("def g = { String a, String... rest -> [a, rest.size()] }\ng('x', 'y', 'z')"),
        "[x, 2]"
    );
}
