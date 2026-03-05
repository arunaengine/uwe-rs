//! Runs small workflows with factories and operators and checks what `view` prints against
//! the experiments of the channels spec.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_evaluator::interpreter::Host;
use nextflow_evaluator::lower::lower_module;
use nextflow_parser::parse_script;

#[derive(Default)]
struct Output(String);

impl Host for Output {
    fn print(&mut self, text: &str) {
        self.0.push_str(text);
    }
}

/// What the entry workflow prints, or the error.
fn run(body: &str) -> Result<String, String> {
    let source = format!("workflow {{\n{body}\n}}");
    let parsed = parse_script(&source);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let module = Rc::new(lower_module(&parsed.tree, &source).expect("lowers"));
    let mut output = Output::default();
    nextflow_runtime::run(&module, &mut output).map_err(|error| error.to_string())?;
    Ok(output.0)
}

fn lines(body: &str) -> Vec<String> {
    run(body).expect("runs").lines().map(String::from).collect()
}

#[test]
fn factories() {
    assert_eq!(lines("channel.of(1, 2, 3).view()"), ["1", "2", "3"]);
    assert_eq!(
        lines("channel.of(1..3, [4, 5]).view()"),
        ["1", "2", "3", "[4, 5]"]
    );
    assert_eq!(lines("channel.fromList([1, 2]).view()"), ["1", "2"]);
    assert_eq!(lines("channel.value(5).view()"), ["5"]);
    assert_eq!(lines("channel.empty().view()"), Vec::<String>::new());
    let error = run("channel.of(channel.of(1))").unwrap_err();
    assert_eq!(
        error,
        "IllegalArgumentException: Argument of `channel.of` cannot be a channel object"
    );
}

#[test]
fn values_one_at_a_time() {
    assert_eq!(
        lines("channel.of(1, 2, 3).map { v -> v * 2 }.view()"),
        ["2", "4", "6"]
    );
    assert_eq!(
        lines("channel.of(1, 2, 3).map { v -> v == 2 ? null : v }.view()"),
        ["1", "3"]
    );
    assert_eq!(
        lines("channel.of(1, 2, 3).filter { v -> v > 1 }.view()"),
        ["2", "3"]
    );
    assert_eq!(
        lines("channel.of('a1', 'b', 'c2').filter(~/.\\d/).view()"),
        ["a1", "c2"]
    );
}

#[test]
fn flattening() {
    assert_eq!(
        lines("channel.of([a: 1, b: 2]).flatMap { v -> v }.view()"),
        ["a=1", "b=2"]
    );
    assert_eq!(
        lines("channel.of(1, null, 2).flatMap { v -> v }.view()"),
        ["1", "2"]
    );
    assert_eq!(
        lines("channel.of([1, [2, [3]]], [a: 1]).flatten().view()"),
        ["1", "2", "3", "[a:1]"]
    );
    assert_eq!(lines("channel.of(1, null, 2).flatten().view()"), ["1", "2"]);
}

#[test]
fn gathering() {
    assert_eq!(lines("channel.of(1, 2, 3).collect().view()"), ["[1, 2, 3]"]);
    assert_eq!(
        lines("channel.of([1, [2, 3]], [4]).collect().view()"),
        ["[1, [2, 3], 4]"]
    );
    assert_eq!(
        lines("channel.of(1, 2, 3).collect { v -> [v, [v]] }.view()"),
        ["[1, [1], 2, [2], 3, [3]]"]
    );
    assert_eq!(
        lines("channel.empty().collect().view()"),
        Vec::<String>::new()
    );
    assert_eq!(
        lines("channel.empty().collect().ifEmpty('DEF').view()"),
        ["DEF"]
    );
    assert_eq!(lines("channel.empty().toList().view()"), ["[]"]);
    assert_eq!(lines("channel.value(3).toList().view()"), ["[3]"]);
    assert_eq!(lines("channel.of(4, 5).first().view()"), ["4"]);
    assert_eq!(
        lines("channel.of(1, 2).mix(channel.of(3)).toList().view()"),
        ["[1, 2, 3]"]
    );
}

#[test]
fn pipes_and_waiting() {
    assert_eq!(
        lines("channel.of(1, 2) | map { v -> v * 10 } | view"),
        ["10", "20"]
    );
    let error = run("channel.of(1, 2, 3).first { v -> v > 10 }.view()").unwrap_err();
    let message =
        "The run cannot finish, because these steps wait for input that never comes: view";
    assert_eq!(error, format!("IllegalStateException: {message}"));
}
