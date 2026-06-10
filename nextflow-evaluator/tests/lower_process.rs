//! Checks the lowering of processes in the legacy syntax that nf-core modules use.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_evaluator::lower::lower_module;
use nextflow_evaluator::lowered::{ExpressionKind, Input, Module, OutputPart};
use nextflow_parser::ast::BodyKind;
use nextflow_parser::{LineIndex, parse_script};

/// The nf-core GUNZIP module, the first target of the Nextflow runtime design.
const GUNZIP: &str = include_str!("fixtures/gunzip.nf");

fn lower(source: &str) -> Result<Module, Vec<String>> {
    let parsed = parse_script(source);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let lines = LineIndex::new(source);
    lower_module(&parsed.tree, source).map_err(|errors| {
        errors
            .iter()
            .map(|error| lines.locate(error).to_string())
            .collect()
    })
}

fn process(sections: &str) -> String {
    format!("process P {{\n{sections}\n}}")
}

#[test]
fn gunzip_lowers() {
    let module = lower(GUNZIP).expect("lowers");
    let [process] = module.processes.as_slice() else {
        panic!("one process")
    };
    assert_eq!(&*process.name, "GUNZIP");
    let names: Vec<&str> = process.directives.iter().map(|d| &*d.name).collect();
    assert_eq!(names, ["tag", "label", "conda", "container"]);
    let [Input::Tuple(parts)] = process.inputs.as_slice() else {
        panic!("a tuple input")
    };
    assert!(matches!(
        parts.as_slice(),
        [Input::Value { .. }, Input::Path { slot: Some(_), .. }]
    ));
    let [files, versions] = process.outputs.as_slice() else {
        panic!("two outputs")
    };
    assert_eq!(files.emit.as_deref(), Some("gunzip"));
    assert!(matches!(
        files.parts.as_slice(),
        [OutputPart::Value(_), OutputPart::Path { .. }]
    ));
    assert_eq!(versions.topic.as_deref(), Some("versions"));
    assert_eq!(versions.emit.as_deref(), Some("versions_gunzip"));
    assert!(matches!(
        versions.parts.as_slice(),
        [.., OutputPart::Eval(_)]
    ));
    assert!(process.when.is_some() && process.stub.is_some());
    assert_eq!(
        process.body.as_ref().map(|body| body.kind),
        Some(BodyKind::Script)
    );
}

#[test]
fn inputs_and_outputs() {
    let source = process(
        "input:\n    path (indexes), stageAs: 'dir*'\n    each mode\n    path 'x.txt'\n    stdin\n\
         output:\n    path 'out.txt', optional: true, emit: out\n    stdout\n    env('FOO'), emit: foo\n\
         script:\n    \"${indexes} ${mode}\"",
    );
    let module = lower(&source).expect("lowers");
    let process = &module.processes[0];
    assert!(matches!(
        process.inputs.as_slice(),
        [
            Input::Path { slot: Some(_), .. },
            Input::Each(_),
            Input::Path { slot: None, .. },
            Input::Stdin
        ]
    ));
    let [out, stdout, env] = process.outputs.as_slice() else {
        panic!("three outputs")
    };
    assert!(out.optional.is_some() && out.emit.as_deref() == Some("out"));
    assert!(matches!(stdout.parts.as_slice(), [OutputPart::Stdout]));
    assert!(matches!(env.parts.as_slice(), [OutputPart::Env(name)] if &**name == "FOO"));
}

#[test]
fn errors() {
    let errors = |sections: &str| lower(&process(sections)).expect_err("fails");
    assert_eq!(
        errors("    foo 1\n    script:\n    'x'"),
        ["2:5: Unrecognized process directive `foo`"]
    );
    assert_eq!(
        errors("    input:\n    value x\n    script:\n    'x'"),
        ["3:5: Unrecognized process input qualifier `value`"]
    );
    assert_eq!(
        errors("    script:\n    task = 1\n    'x'"),
        ["3:5: Built-in constant or namespace cannot be re-assigned"]
    );
    // Nextflow 26.04.6 fails on `optional true` after an output too: "Cannot invoke method
    // optional() on null object".
    assert_eq!(
        errors("    output:\n    path '*.pdf' optional true\n    script:\n    'x'"),
        ["3:5: Invalid process output"]
    );
    // A variable that the script sets without `def` is a task variable, which outputs can read.
    assert!(
        lower(&process(
            "    output:\n    path(\"${out}\")\n    script:\n    out = 'a.txt'\n    ''"
        ))
        .is_ok()
    );
}

/// The errors of strict syntax for the legacy syntax of a script that lowers.
fn legacy(source: &str) -> Vec<String> {
    let module = lower(source).expect("lowers");
    let lines = LineIndex::new(source);
    module
        .legacy
        .iter()
        .map(|error| lines.locate(error).to_string())
        .collect()
}

#[test]
fn legacy_syntax() {
    // A directive looks up an unknown name when a task runs.
    let source = process("    cpus y\n    script:\n    'x'");
    assert_eq!(legacy(&source), ["2:10: `y` is not defined"]);
    // Strict syntax resolves a bare `env` name as a variable; the legacy parser takes it as
    // the name.
    let source = process("    output:\n    env LINES\n    script:\n    'x'");
    assert_eq!(legacy(&source), ["3:9: `LINES` is not defined"]);
    let source = process("    script:\n    [1].each { v -> total = v }\n    'x'");
    assert_eq!(
        legacy(&source),
        ["3:21: Variables in a closure should be declared with `def`"]
    );
    let inputs = "    input:\n    tuple val(meta), path(a)\n    tuple val(meta), path(b)\n    \
                  script:\n    'x'";
    assert_eq!(
        legacy(&process(inputs)),
        ["4:15: `meta` is already declared"]
    );
    // An `if` among the directives runs for each task.
    let setup = "    if (workflow.profile == 'conda') {\n        exit 1, 'no conda'\n    }\n    \
                 cpus 2\n    script:\n    'x'";
    let source = process(setup);
    assert_eq!(legacy(&source), ["2:5: Invalid process directive"]);
    let module = lower(&source).expect("lowers");
    assert_eq!(module.processes[0].setup.len(), 1);
    assert_eq!(module.processes[0].directives.len(), 1);
    // Strict syntax wants a string as an `env` name; the legacy parser takes a bare name.
    let env = "    output:\n    env LINES\n    script:\n    'x'";
    assert_eq!(legacy(&process(env)), ["3:9: `LINES` is not defined"]);
    // A bare name that is no variable is a file name.
    let output = "    output:\n    path(bwa), emit: index\n    script:\n    'x'";
    let source = process(output);
    assert_eq!(legacy(&source), ["3:10: `bwa` is not defined"]);
    let module = lower(&source).expect("lowers");
    assert!(matches!(
        module.processes[0].outputs[0].parts.as_slice(),
        [OutputPart::Path { pattern, .. }]
            if matches!(&pattern.kind, ExpressionKind::Literal(value) if value.to_string() == "bwa")
    ));
}

#[test]
fn closures_may_declare_again_what_a_binding_holds() {
    let source = "workflow W {\n    take:\n    fasta\n    main:\n    \
                  fasta.map { meta, fasta -> fasta }\n    \
                  out = fasta\n    out.map { out -> out }\n}\n";
    assert_eq!(
        legacy(source),
        [
            "5:23: `fasta` is already declared",
            "7:15: `out` is already declared"
        ]
    );
    // A variable declared with `def` stays an error, as in Groovy.
    let source = "def f() {\n    def x = 1\n    [1].each { x -> x }\n}\n";
    assert_eq!(
        lower(source).expect_err("fails"),
        ["3:16: `x` is already declared"]
    );
}
