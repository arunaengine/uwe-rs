//! Runs the Python programs of `tests/fixtures/python` and compares their output with what
//! CPython 3.11 printed for them, recorded in the `.out` file next to each program.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;
use std::rc::Rc;

use python_evaluator::{Interpreter, LocalHost};

fn run(path: &Path) -> String {
    let text = std::fs::read_to_string(path).expect("reads the program");
    let host = Rc::new(LocalHost::new(path.parent().expect("a folder")));
    let mut interpreter = Interpreter::new(host.clone());
    let result = interpreter.run_text(&path.to_string_lossy(), &text);
    let mut output = host.output.borrow().clone();
    if let Err(error) = result {
        output.push_str(&format!("{error}\n"));
    }
    output
}

#[test]
fn programs_print_what_python_prints() {
    let folder = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/python");
    let mut programs: Vec<_> = std::fs::read_dir(&folder)
        .expect("reads the fixtures")
        .map(|entry| entry.expect("an entry").path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "py"))
        .collect();
    programs.sort();
    assert!(!programs.is_empty());
    for program in programs {
        let expected =
            std::fs::read_to_string(program.with_extension("out")).expect("reads the output");
        let actual = run(&program);
        assert_eq!(actual, expected, "{}", program.display());
    }
}

fn evaluate(code: &str) -> Result<String, python_evaluator::Error> {
    let host = Rc::new(LocalHost::new(std::env::temp_dir()));
    let mut interpreter = Interpreter::new(host.clone());
    interpreter.run_text("<test>", code)?;
    let output = host.output.borrow().clone();
    Ok(output)
}

#[test]
fn errors_name_the_line_and_the_exception() {
    let error = evaluate("x = 1\ny = {}\ny['missing']\n").expect_err("a KeyError");
    assert_eq!(error.to_string(), "<test>:3: KeyError: 'missing'");
    assert!(!error.is_unsupported());
}

#[test]
fn unsupported_features_are_not_caught_by_except() {
    let error = evaluate("import os\ntry:\n    os.fork()\nexcept Exception:\n    pass\n")
        .expect_err("unsupported");
    assert!(error.is_unsupported(), "{error}");
}

#[test]
fn writes_stay_in_memory() {
    let output = evaluate(
        "with open('iunctus-test-never-written.txt', 'w') as f:\n    f.write('hi')\nprint(open('iunctus-test-never-written.txt').read())\n",
    )
    .expect("runs");
    assert_eq!(output, "hi\n");
    assert!(
        !std::env::temp_dir()
            .join("iunctus-test-never-written.txt")
            .exists()
    );
}

#[test]
fn loops_that_never_end_stop() {
    let host = Rc::new(LocalHost::new(std::env::temp_dir()));
    let mut interpreter = Interpreter::new(host);
    interpreter.step_limit = 10_000;
    let error = interpreter
        .run_text("<test>", "while True:\n    pass\n")
        .err()
        .expect("stops");
    assert!(error.is_unsupported());
}

#[test]
fn recursion_is_limited() {
    let error = evaluate("def f(n):\n    return f(n + 1)\nf(0)\n").expect_err("too deep");
    assert!(error.to_string().contains("RecursionError"), "{error}");
}

#[test]
fn yaml_follows_the_safe_loader() {
    let output = evaluate(
        "import yaml\nprint(yaml.safe_load('a: yes\\nb: 1e3\\nc: 1.0e+3\\nd: 010\\ne: 1:30\\nf: ~\\ng: [1, x]\\nh: &x {k: 1}\\ni: *x\\n'))\n",
    )
    .expect("runs");
    assert_eq!(
        output,
        "{'a': True, 'b': '1e3', 'c': 1000.0, 'd': 8, 'e': 90, 'f': None, 'g': [1, 'x'], 'h': {'k': 1}, 'i': {'k': 1}}\n"
    );
}
