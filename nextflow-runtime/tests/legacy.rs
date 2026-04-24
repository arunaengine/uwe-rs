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

/// Legacy includes may name the project directory in the source and break the line
/// before `as`.
#[test]
fn includes_read_the_project_directory() {
    let root = tempfile::tempdir().expect("a temporary directory");
    std::fs::create_dir(root.path().join("modules")).expect("creates modules");
    write(
        root.path(),
        "modules/show.nf",
        "workflow SHOW {\n    main:\n    println('shown')\n}\n",
    );
    write(
        root.path(),
        "main.nf",
        "include { SHOW\n    as SHOW_IT } from \"${projectDir}/modules/show\"\nworkflow {\n    SHOW_IT()\n}\n",
    );
    let module = Loader::default()
        .load(&root.path().join("main.nf"))
        .expect("loads");
    let mut output = Output::default();
    nextflow_runtime::run(&module, &mut output).expect("runs");
    assert_eq!(output.0, "shown\n");
}

/// Legacy processes may set variables among their sections, which later sections read, and
/// an assignment among the directives sets a variable, not the directive. Checked with
/// Nextflow 26.04.6 and `NXF_SYNTAX_PARSER=v1`.
#[test]
fn process_sections_set_variables() {
    let root = tempfile::tempdir().expect("a temporary directory");
    write(
        root.path(),
        "main.nf",
        "process P {\n    errorStrategy = 'ignore'\n    echo true\n\n    input:\n    val x\n\n    \
         output:\n    path 'out.txt', emit: out\n\n    def y = 7\n    script:\n    \
         \"echo $x $y $errorStrategy\"\n}\nworkflow {\n    P(channel.of(3))\n}\n",
    );
    let module = Loader::default()
        .load(&root.path().join("main.nf"))
        .expect("loads");
    let options = nextflow_runtime::Options::default();
    let (_, requests) =
        nextflow_runtime::Run::start(&module, &options, &mut Output::default()).expect("starts");
    assert_eq!(requests[0].script, "#!/bin/bash -ue\necho 3 7 ignore\n");
    let error_strategy = requests[0]
        .directives
        .iter()
        .find(|directive| &*directive.name == "errorStrategy");
    assert!(error_strategy.is_none(), "{error_strategy:?}");
}
