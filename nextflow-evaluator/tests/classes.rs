//! Runs Groovy classes like those of the `lib/` directory of older nf-core pipelines, and legacy
//! loops, against the results of Groovy 4.0.31, which Nextflow 26.04.6 bundles.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::rc::Rc;

use nextflow_evaluator::interpreter::{Host, Interpreter};
use nextflow_evaluator::lower::lower_module;
use nextflow_evaluator::lowered::{Library, LibraryClass};
use nextflow_parser::parse_script;

struct Output;

impl Host for Output {
    fn print(&mut self, _: &str) {}
}

/// Runs the function `f` of a script whose classes form the library, as the loader makes it.
fn run(source: &str) -> Result<String, String> {
    let parsed = parse_script(source);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let module = Rc::new(lower_module(&parsed.tree, source).expect("lowers"));
    let library = Rc::new(Library::default());
    for def in &module.classes {
        let class = LibraryClass {
            def: def.clone(),
            module: module.clone(),
            statics: RefCell::new(None),
            values: RefCell::new(None),
        };
        library
            .classes
            .borrow_mut()
            .insert(def.name.clone(), Rc::new(class));
    }
    *module.library.borrow_mut() = Some(library);
    let mut output = Output;
    let mut interpreter = Interpreter::new(&mut output);
    interpreter
        .call_function(&module, "f", Vec::new())
        .map(|value| value.to_string())
        .map_err(|error| error.to_string())
}

const CLASSES: &str = "
class Constants {
    static List NAMES = ['a', 'b']
    static List MORE = Constants.NAMES + ['c']
    static Integer COUNT = 3L

    static enum Mode {
        FAST,
        SLOW,
    }

    static Map INPUT = [
        key: [Mode.FAST, 'x'],
    ]
}

class Utils {
    public static getMode(text) {
        try {
            return Constants.Mode.valueOf(text.toUpperCase())
        } catch (IllegalArgumentException e) {
            return null
        }
    }

    public static label(Map options, value) {
        return \"${options.prefix}${value}\"
    }

    public static label(value) {
        label([prefix: '-'], value)
    }

    public static String join(String separator, ... parts) {
        return parts.join(separator)
    }

    public static Long total(items) {
        def sum = 0
        for (item in items) {
            if (item == 2) {
                continue
            }
            sum += item
        }
        for (int i = 0; i < 3; i++) {
            sum += i
        }
        def n = 0
        while (true) {
            n++
            if (n > 4) break
        }
        return sum + n
    }

    public static names() {
        this.namesOf(Constants.Mode)
    }

    public static namesOf(e) {
        e.values()*.name()*.toLowerCase()
    }
}

class Counter {
    def count = 10
    def step

    Counter(step) {
        this.step = step
    }

    def next() {
        count += step
        return count
    }

    def getDouble() {
        return count * 2
    }
}
";

#[test]
fn matches_groovy_classes() {
    let source = format!(
        "{CLASSES}
def f() {{
    def c = new Counter(5)
    c.next()
    [
        Constants.MORE,
        Constants.COUNT.getClass().getSimpleName(),
        Utils.getMode('slow'),
        Utils.getMode('slow') == Constants.Mode.SLOW,
        Utils.getMode('other'),
        Constants.Mode.SLOW.ordinal(),
        Constants.INPUT.key[0] in [Constants.Mode.FAST],
        Utils.label(1),
        Utils.label(2, prefix: '+'),
        Utils.join('/', 'a', 'b', 'c'),
        Utils.total([1, 2, 3]),
        Utils.total([1]).getClass().getSimpleName(),
        Utils.names(),
        [(Constants.Mode.FAST): 1].keySet().toList()[0].name(),
        c.count,
        c.double,
    ].toString()
}}
"
    );
    // Groovy 4.0.31 prints this for the same classes and function.
    assert_eq!(
        run(&source).expect("runs"),
        "[[a, b, c], Integer, SLOW, true, null, 1, true, -1, +2, a/b/c, 12, Long, [fast, slow], FAST, 15, 30]"
    );
}

#[test]
fn rejects_missing_members() {
    let source = format!("{CLASSES}\ndef f() {{ Utils.nothing(1) }}\n");
    assert_eq!(
        run(&source).unwrap_err(),
        "MissingMethodException: No signature of method: Utils.nothing() is \
         applicable for argument types: (Integer) values: [1]"
    );
    let source = format!("{CLASSES}\ndef f() {{ Constants.OTHER }}\n");
    assert_eq!(
        run(&source).unwrap_err(),
        "MissingPropertyException: No such property: OTHER for class: Constants"
    );
}

#[test]
fn selects_specific_overloads() {
    for methods in [
        "static pick(Long x) { 'long' }; static pick(Integer x) { 'integer' }",
        "static pick(Integer x) { 'integer' }; static pick(Long x) { 'long' }",
    ] {
        assert_eq!(
            run(&format!(
                "class Numeric {{ {methods} }}\n\
                          def f() {{ [Numeric.pick(1), Numeric.pick(1L)] }}"
            ))
            .unwrap(),
            "[integer, long]"
        );
    }
    assert_eq!(
        run(
            "class Numeric { static wide(Long x) { x.getClass().getSimpleName() } }\n\
             def f() { Numeric.wide(1) }"
        )
        .unwrap(),
        "Long"
    );
    let error = run("class Numeric { static pick(Integer a, Long b) { 1 }; \
                     static pick(Long a, Integer b) { 2 } }\n\
                     def f() { Numeric.pick(1, 1) }")
    .unwrap_err();
    assert!(
        error.starts_with("GroovyRuntimeException: Ambiguous method overloading"),
        "{error}"
    );
}

#[test]
fn preserves_parameter_types() {
    assert_eq!(
        run("class Typed { static value(Integer count) { count = 5L;\n\
             { count += 2L }.call()\n count.getClass().getSimpleName() } }\n\
             def f() { Typed.value(1) }")
        .unwrap(),
        "Integer"
    );
}

#[test]
fn preserves_import_aliases() {
    let source = "import static java.lang.Math.max as min\n\
                  import static java.lang.Math.min as smallest\n\
                  def f() { [min(1, 2), smallest(1, 2)] }\n";
    assert_eq!(run(source).expect("runs"), "[2, 1]");
}

const SWITCHES: &str = r#"
def size(text) {
    def number
    switch (text[-1]) {
        case 'k':
        case 'K':
            number = 1000
            break
        case 'm':
            number = 1000000; break
        default:
            number = 1
    }
    number
}

def kind(value) {
    switch (value) {
        case null: 'null'; break
        case String: 'string'; break
        case [1, 2]: 'small'; break
        case { it > 10 }: 'big'; break
        case ~/x+/: 'never'; break
        default: 'other'
    }
}

def fall(value) {
    def seen = []
    switch (value) {
        case 'a':
            seen << 'a'
        case 'b':
            seen << 'b'
            break
        case 'c':
            seen << 'c'
        default:
            seen << 'default'
    }
    seen
}

def script(source) {
    switch (source) {
        case 'fastq':
            """fastq ${source}"""
            break;
        case 'bam':
            "bam"
            break
    }
}

def loop() {
    def out = []
    for (i in 1..4) {
        switch (i) {
            case 2: continue
            case 3: out << 'three'; break
            default: out << i
        }
    }
    out
}

def f() {
    [size('2k'), size('3m'), size('4'), kind(null), kind('s'), kind(2), kind(11), kind(5),
     fall('a'), fall('z'), fall('c'), script('fastq'), script('bam'), script('x'), loop(),
     "${'k'}" == 'k'].toString()
}
"#;

#[test]
fn matches_groovy_switch() {
    // Groovy 4.0.31 prints this for the same functions.
    assert_eq!(
        run(SWITCHES).expect("runs"),
        "[1000, 1000000, 1, null, string, small, big, other, [a, b], [default], [c, default], \
         fastq fastq, bam, null, [1, three, 4], true]"
    );
}
