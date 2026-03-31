//! Scripts of legacy syntax, which strict syntax rejects and iunctus runs with the meaning of
//! the legacy parser (decision 14).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use nextflow_evaluator::interpreter::Host;
use nextflow_runtime::Loader;

#[derive(Default)]
struct Output(String);

impl Host for Output {
    fn print(&mut self, text: &str) {
        self.0.push_str(text);
    }
}

fn write(root: &Path, name: &str, text: &str) {
    std::fs::write(root.join(name), text).expect("writes the script");
}

#[test]
fn statements_next_to_declarations_run_when_the_script_loads() {
    let root = tempfile::tempdir().expect("a temporary directory");
    write(
        root.path(),
        "main.nf",
        "def greeting = 'hello'
if (greeting) {
    names = ['a', 'b']
}
include { SHOW } from './show'
println \"main: ${greeting}\"
def shout(text) { text.toUpperCase() + names.size() }
workflow {
    SHOW(channel.fromList(names))
    println(shout(greeting))
}
",
    );
    write(
        root.path(),
        "show.nf",
        "def prefix = 'item'
println 'show loaded'
workflow SHOW {
    take:
    ch
    main:
    ch.map { v -> \"${prefix} ${v}\" }.view()
}
",
    );
    let module = Loader::default()
        .load(&root.path().join("main.nf"))
        .expect("loads");
    let mut output = Output::default();
    nextflow_runtime::run(&module, &mut output).expect("runs");
    assert_eq!(
        output.0,
        "main: hello\nshow loaded\nHELLO2\nitem a\nitem b\n"
    );
}
