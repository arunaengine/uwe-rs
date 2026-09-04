//! Runs small scripts and checks the results against Nextflow 26.04.6 (Groovy spec, sections 8 and 9).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_evaluator::interpreter::{Host, Interpreter};
use nextflow_evaluator::lower::lower_module;
use nextflow_parser::parse_script;

#[derive(Default)]
struct Output(String);

impl Host for Output {
    fn print(&mut self, text: &str) {
        self.0.push_str(text);
    }
}

/// Runs the function `f` of the script: its result as text, or the error and printed output.
fn run_with_output(source: &str) -> (Result<String, String>, String) {
    let parsed = parse_script(source);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let module = Rc::new(lower_module(&parsed.tree, source).expect("lowers"));
    let mut output = Output::default();
    let mut interpreter = Interpreter::new(&mut output);
    let result = interpreter
        .call_function(&module, "f", Vec::new())
        .map(|value| value.to_string())
        .map_err(|error| error.to_string());
    (result, output.0)
}

fn run(source: &str) -> String {
    run_with_output(source).0.expect("runs")
}

fn fails(source: &str) -> String {
    run_with_output(source).0.expect_err("fails")
}

#[test]
fn last_statement_is_the_result() {
    assert_eq!(
        run("def f() { g(0) }\ndef g(x) { if( x ) { 'yes' } else { 'no' } }"),
        "no"
    );
    assert_eq!(
        run("def f() { g(0) }\ndef g(x) { if( x ) { 'yes' } }"),
        "null"
    );
    assert_eq!(run("def f() { def x = 1 }"), "1");
    assert_eq!(
        run("def f() { try { 1 / 0 } catch( e ) { 'caught' } }"),
        "caught"
    );
    assert_eq!(run("def f() { return 2\n 3 }"), "2");
}

#[test]
fn typed_parameters_and_returns_keep_supported_conversions() {
    assert_eq!(
        run("def wide(x: Long) { x.getClass().getSimpleName() }\n\
             def text() -> String { return 5 }\n\
             def defaulted(x: Integer = 5L) -> Long { x }\n\
             def f() { [wide(5), text() + 1, defaulted().getClass().getSimpleName(), \
             { x: Long -> x.getClass().getSimpleName() }.call(5)] }"),
        "[Long, 51, Long, Long]"
    );
    for argument in ["5L", "'a'"] {
        assert!(
            fails(&format!(
                "def typed(x: Integer) {{ x }}\ndef f() {{ typed({argument}) }}"
            ))
            .starts_with("MissingMethodException:"),
            "{argument}"
        );
    }
    assert!(
        fails("def f() { { x: Integer -> x }.call(5L) }").starts_with("MissingMethodException:")
    );
}

#[test]
fn script_numeric_overloads_use_specificity_in_both_orders() {
    for methods in [
        "def pick(x: Long) { 'long' }\ndef pick(x: Integer) { 'integer' }",
        "def pick(x: Integer) { 'integer' }\ndef pick(x: Long) { 'long' }",
    ] {
        assert_eq!(
            run(&format!("{methods}\ndef f() {{ [pick(1), pick(1L)] }}")),
            "[integer, long]"
        );
    }
    assert_eq!(
        run(
            "def pick(x: Double) { 'double' }\ndef pick(x: Long) { 'long' }\n\
             def f() { pick(1) }"
        ),
        "long"
    );
    assert!(
        fails(
            "def pick(a: Integer, b: Long) { 1 }\ndef pick(a: Long, b: Integer) { 2 }\n\
               def f() { pick(1, 1) }"
        )
        .starts_with("GroovyRuntimeException: Ambiguous method overloading")
    );
}

#[test]
fn typed_locals_convert_initialization_assignments_and_captured_writes() {
    for declaration in ["def text: String", "String text"] {
        assert_eq!(
            run(&format!("def f() {{ {declaration} = 5\n text + 1 }}")),
            "51"
        );
        assert_eq!(
            run(&format!(
                "def f() {{ {declaration}\n text = 7\n\
                          def change = {{ {{ text = 8 }}.call() }}\n\
                          change.call()\n [text + 1, text.getClass().getSimpleName()] }}"
            )),
            "[81, String]"
        );
    }
    assert_eq!(
        run("def f() { def count: Integer = 5L\n count += 2L\n\
             def changed = (count = 9L)\n count++\n\
             [count, count.getClass().getSimpleName(), changed.getClass().getSimpleName()] }"),
        "[10, Integer, Integer]"
    );
    assert_eq!(
        run("def f() { def (text: String, count: Integer) = [5, 6L]\n\
             (text, count) = [7, 8L]\n [text + 1, count.getClass().getSimpleName()] }"),
        "[71, Integer]"
    );
    assert_eq!(
        run("def f() { def String first = 1, second = 2\n [first + 3, second + 4] }"),
        "[13, 24]"
    );
}

#[test]
fn typed_reassignments_preserve_numeric_overload_identity() {
    assert_eq!(
        run(
            "def pick(x: Long) { 'long' }\ndef pick(x: Integer) { 'integer' }\n\
             def typed(x: Integer) { x = 7L;\n { x += 2L }.call()\n pick(x) }\n\
             def f() { def count: Integer = 5L;\n { count = 8L }.call()\n\
             [pick(count), typed(1), { value: Integer -> value = 2L\n pick(value) }.call(1)] }"
        ),
        "[integer, integer, integer]"
    );
}

#[test]
fn typed_loop_and_module_variables_preserve_scalar_types() {
    assert_eq!(
        run(
            "def f() { def result = []\n for (String item in [5, 6]) { result << item + 1 }\n\
             for (Integer item in [7L]) { result << item.getClass().getSimpleName() }\n result }"
        ),
        "[51, 61, Integer]"
    );
    let source = "String label = 5\ndef f() { def initial = label + 1;\n\
                  { label = 7 }.call()\n [initial, label + 1] }";
    let parsed = parse_script(source);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let module = Rc::new(lower_module(&parsed.tree, source).expect("lowers"));
    let mut output = Output::default();
    let mut interpreter = Interpreter::new(&mut output);
    interpreter.start_statements(&module).unwrap();
    assert_eq!(
        interpreter
            .call_function(&module, "f", Vec::new())
            .unwrap()
            .to_string(),
        "[51, 71]"
    );
}

#[test]
fn closures_share_captured_variables() {
    let source = "def f() {\n def n = 10\n def c = { v -> v + n }\n n = 20\n c.call(1)\n}";
    assert_eq!(run(source), "21");
    let source = "def f() {\n def total = 0\n def add = { v -> total += v }\n add.call(2)\n add.call(3)\n total\n}";
    assert_eq!(run(source), "5");
    assert_eq!(run("def f() { { a, b = 5 -> a + b }.call(1) }"), "6");
    assert_eq!(run("def f() { { -> 5 }.call(1) }"), "5");
    assert_eq!(run("def f() { { it }.call() }"), "null");
}

#[test]
fn argument_adaptation() {
    assert_eq!(
        run("def f() { { a, b = 5 -> \"${a}/${b}\" }.call([1, 2]) }"),
        "[1, 2]/5"
    );
    assert_eq!(
        run("def f() { { a, b, c = 5 -> \"${a}/${b}/${c}\" }.call([1, 2]) }"),
        "1/2/5"
    );
    assert_eq!(
        run("def f() { { a, b -> a + b }.call(tuple('s', 1)) }"),
        "s1"
    );
    let error = fails("def f() { { a, b -> a }.call([1, 2, 3]) }");
    assert!(
        error.starts_with("MissingMethodException: No signature of method"),
        "{error}"
    );
    assert!(fails("def f() { { it }.call(1, 2) }").starts_with("MissingMethodException"));
    assert_eq!(run("def f() { g([1, 2]) }\ndef g(a, b) { a + b }"), "3");
    let source = "def f() { g(1, k: 'v') + ' ' + g(k: 'v', 2) }\ndef g(m, x) { \"${m}|${x}\" }";
    assert_eq!(run(source), "[k:v]|1 [k:v]|2");
    assert_eq!(
        run("def f() { g(1) }\ndef g(a, b = a + 1) { [a, b] }"),
        "[1, 2]"
    );
}

#[test]
fn logical_operators() {
    assert_eq!(run("def f() { 'a' && 'b' }"), "true");
    assert_eq!(
        run("def f() { [0 ?: 5, '' ?: 'x', 'y' ?: 'z'] }"),
        "[5, x, y]"
    );
    assert_eq!(run("def f() {\n def x = 0\n x ?= 7\n x\n}"), "7");
    assert_eq!(run("def f() { !1 == false }"), "true");
    assert_eq!(
        run("def f() { 'docker' in ['singularity', 'apptainer'] }"),
        "false"
    );
    assert_eq!(run("def f() { null == null && 1 != 2 }"), "true");
}

#[test]
fn exceptions() {
    let source = "def f() { try { 1 / 0 } catch( ArithmeticException e ) { e.message } }";
    assert_eq!(run(source), "Division by zero");
    let source =
        "def f() { try { throw new IllegalArgumentException('bad') } catch( e ) { e.message } }";
    assert_eq!(run(source), "bad");
    let source =
        "def f() { try { throw new Exception('x') } catch( IllegalStateException e ) { 1 } }";
    assert_eq!(fails(source), "Exception: x");
    assert_eq!(
        fails("def f() { try { assert 1 == 2 } catch( e ) { 1 } }"),
        "AssertionError: assert 1 == 2"
    );
    assert_eq!(
        fails("def f() { error('boom') }"),
        "AbortOperationException: boom"
    );
    let source = "def f() { try { null.x } catch( NullPointerException e ) { e.message } }";
    assert_eq!(run(source), "Cannot get property 'x' on null object");
}

#[test]
fn indexing_and_assignment() {
    assert_eq!(run("def f() { [10, 20, 30][5] }"), "null");
    assert_eq!(run("def f() { [10, 20, 30][-1] }"), "30");
    assert!(fails("def f() { [10, 20, 30][-4] }").contains("IndexOutOfBoundsException"));
    assert_eq!(
        run("def f() {\n def x = [1]\n x[3] = 4\n x\n}"),
        "[1, null, null, 4]"
    );
    assert_eq!(run("def f() { ['abc'[1], 'abc'[-1]] }"), "[b, c]");
    assert!(fails("def f() { 'abc'[5] }").starts_with("StringIndexOutOfBoundsException"));
    assert_eq!(
        run("def f() {\n def (a, b, c) = [1, 2]\n \"${a}${b}${c}\"\n}"),
        "12null"
    );
    assert_eq!(
        run("def f() {\n def m = [a: 1]\n m.b = m.a + 1\n m['c'] = 3\n m\n}"),
        "[a:1, b:2, c:3]"
    );
    assert_eq!(
        run("def f() {\n def x = [1]\n def y = x\n x += [2]\n [x, y]\n}"),
        "[[1, 2], [1]]"
    );
    assert_eq!(run("def f() {\n def m = [n: 1]\n m.n += 1\n m\n}"), "[n:2]");
    assert_eq!(
        run("def f() { [[a: 1], [a: 2], [b: 3]]*.a }"),
        "[1, 2, null]"
    );
    assert_eq!(run("def f() { null?.a }"), "null");
}

#[test]
fn strings_and_output() {
    let (result, output) =
        run_with_output("def f() {\n println('a')\n println([id: 'S1', n: 2])\n print(1.50)\n}");
    assert_eq!(result.as_deref(), Ok("null"));
    assert_eq!(output, "a\n[id:S1, n:2]\n1.50");
    assert_eq!(
        run("def f() { \"${ \"${1}\" instanceof String }\" }"),
        "false"
    );
    assert_eq!(run("def f() { \"${1}\" instanceof String }"), "true");
    assert_eq!(run("def f() { 'a' + 1 + 2 }"), "a12");
    assert_eq!(
        run("def f() { ['a' as String, 1 as String, '42' as Integer, 7 as Boolean] }"),
        "[a, 1, 42, true]"
    );
}
