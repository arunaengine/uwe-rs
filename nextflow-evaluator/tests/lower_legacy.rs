//! Checks that lowering gives legacy code the meaning of Nextflow's legacy parser, as
//! `NXF_SYNTAX_PARSER=v1` runs it, and lists it in `Module::legacy` with the strict error.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_evaluator::lower::lower_module;
use nextflow_evaluator::lowered::{ExpressionKind, Input, Module, OutputPart, Place};
use nextflow_parser::{LineIndex, parse_script};

/// The module, with the legacy syntax it has as located messages.
fn lower(source: &str) -> Result<(Module, Vec<String>), Vec<String>> {
    let parsed = parse_script(source);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let lines = LineIndex::new(source);
    let locate = |errors: &[nextflow_parser::Diagnostic]| -> Vec<String> {
        errors
            .iter()
            .map(|error| lines.locate(error).to_string())
            .collect()
    };
    match lower_module(&parsed.tree, source) {
        Ok(module) => {
            let legacy = locate(&module.legacy);
            Ok((module, legacy))
        }
        Err(errors) => Err(locate(&errors)),
    }
}

const SHADOWED: &str = "process P {
    input:
    tuple val(meta), path(index)
    tuple val(meta), val(other)

    output:
    val(meta)

    script:
    prefix = 'a'
    \"\"\"
    true
    \"\"\"

    stub:
    def index = 'b'
    def prefix = 'c'
    \"\"\"
    true
    \"\"\"
}";

#[test]
fn locals_hide_inputs_and_implicit_variables() {
    let (module, legacy) = lower(SHADOWED).expect("lowers");
    assert_eq!(
        legacy,
        [
            "4:15: `meta` is already declared",
            "16:9: `index` is already declared",
            "17:9: `prefix` is already declared"
        ]
    );
    let process = &module.processes[0];
    // The output reads the second `meta`, as the later input wins.
    let slot = |input: &Input| match input {
        Input::Tuple(parts) => match &parts[0] {
            Input::Value { slot } => *slot,
            _ => panic!("a value input"),
        },
        _ => panic!("a tuple input"),
    };
    let OutputPart::Value(output) = &process.outputs[0].parts[0] else {
        panic!("a value output")
    };
    let ExpressionKind::Variable(place) = &output.kind else {
        panic!("a variable")
    };
    assert_eq!(*place, Place::Local(slot(&process.inputs[1])));
}

#[test]
fn closure_parameters_hide_takes_and_workflow_variables() {
    let source = "workflow W {
    take:
    x

    main:
    channel.of(1).set { vcf }
    y = 5
    channel.of([1, 2]).map { vcf, x -> vcf + x }
    channel.of(7).map { y -> y }
}";
    let (_, legacy) = lower(source).expect("lowers");
    assert_eq!(
        legacy,
        [
            "8:30: `vcf` is already declared",
            "8:35: `x` is already declared",
            "9:25: `y` is already declared"
        ]
    );
}

#[test]
fn locals_still_cannot_be_declared_twice() {
    let source = "def f(a) {\n    def a = 1\n}";
    assert_eq!(lower(source).unwrap_err(), ["2:9: `a` is already declared"]);
}

#[test]
fn undeclared_names_are_looked_up_when_the_code_runs() {
    let (module, legacy) = lower("def f() { segmentation_list.contains('x') }").expect("lowers");
    assert_eq!(legacy, ["1:11: `segmentation_list` is not defined"]);
    let body = format!("{:?}", module.functions[0].body);
    assert!(body.contains("Dynamic(\"segmentation_list\")"), "{body}");
    let (_, legacy) = lower("workflow { UNTAR(channel.of(1)) }").expect("lowers");
    assert_eq!(legacy, ["1:12: `UNTAR` is not defined"]);
}

#[test]
fn bare_env_names_are_names() {
    let source = "process P {
    output:
    env(trim_OT)

    script:
    \"\"\"
    trim_OT=1
    \"\"\"
}";
    let (module, legacy) = lower(source).expect("lowers");
    assert_eq!(legacy, ["3:9: `trim_OT` is not defined"]);
    let OutputPart::Env(name) = &module.processes[0].outputs[0].parts[0] else {
        panic!("an env output")
    };
    assert_eq!(&**name, "trim_OT");
}
