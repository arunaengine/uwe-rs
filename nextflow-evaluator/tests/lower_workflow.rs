//! Checks the lowering of workflows, includes and script-level statements against the rules
//! of `nextflow lint`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_evaluator::lower::{Imports, lower_module, lower_script};
use nextflow_evaluator::lowered::{DefinitionKind, Module};
use nextflow_parser::{LineIndex, parse_script};

const PROCESS: &str = "process P {\n    input:\n    val x\n    script:\n    \"echo ${x}\"\n}\n";

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

#[test]
fn workflows_call_processes_and_emit() {
    let source = format!(
        "{PROCESS}workflow W {{\n    take:\n    x\n    main:\n    P(x)\n    if( x ) {{\n        P.out.set {{ y }}\n    }}\n    emit:\n    out = y\n    x\n}}"
    );
    let module = lower(&source).expect("lowers");
    let workflow = &module.workflows[0];
    assert_eq!(workflow.name.as_deref(), Some("W"));
    let takes: Vec<&str> = workflow.takes.iter().map(|(name, _)| &**name).collect();
    assert_eq!(takes, ["x"]);
    let emits: Vec<&str> = workflow.emits.iter().map(|(name, _)| &**name).collect();
    assert_eq!(emits, ["out", "x"]);
    let single = format!("{PROCESS}workflow W {{\n    main:\n    P(1)\n    emit:\n    P.out\n}}");
    let module = lower(&single).expect("lowers");
    assert_eq!(&*module.workflows[0].emits[0].0, "$out");
}

#[test]
fn dataflow_calls_belong_in_workflows() {
    let errors = lower(&format!("{PROCESS}def f() {{ P(1) }}")).unwrap_err();
    assert_eq!(
        errors,
        ["7:11: Processes can only be called from a workflow"]
    );
    let source = format!("{PROCESS}workflow {{\n    [1].each {{ v -> P(v) }}\n}}");
    assert_eq!(
        lower(&source).unwrap_err(),
        ["8:21: Processes cannot be called from within a closure"]
    );
    let source = format!("{PROCESS}workflow {{\n    channel.of(1) | map {{ v -> v }} | P\n}}");
    assert!(lower(&source).is_ok());
    assert_eq!(
        lower("def f(c) { map { v -> v } }").unwrap_err(),
        ["1:12: `map` is not defined"]
    );
    assert_eq!(
        lower(&format!("{PROCESS}{PROCESS}")).unwrap_err(),
        ["7:9: `P` is already declared"]
    );
}

#[test]
fn statements_and_parameters() {
    let module = lower("params.input = 'x'\nparams.n = 1").expect("lowers");
    let paths: Vec<String> = module
        .parameters
        .iter()
        .map(|(path, _)| path.join("."))
        .collect();
    assert_eq!(paths, ["input", "n"]);
    let module = lower("def x = 1\nprintln(x)").expect("lowers");
    assert!(module.workflows[0].name.is_none());
}

struct Stub;

impl Imports for Stub {
    fn kind(&mut self, source: &str, name: &str) -> Result<DefinitionKind, String> {
        match (source, name) {
            ("./m", "P") => Ok(DefinitionKind::Process),
            ("./m", "helper") => Ok(DefinitionKind::Function),
            _ => Err(format!(
                "Included name '{name}' is not defined in module '{source}'"
            )),
        }
    }
}

#[test]
fn includes() {
    let source = "include { P as Q; helper } from './m'\ninclude { R } from './m'\nworkflow {\n    Q(helper())\n}";
    let parsed = parse_script(source);
    let errors = lower_script(&parsed.tree, source, &mut Stub).unwrap_err();
    assert_eq!(errors.len(), 1);
    assert_eq!(
        errors[0].message,
        "Included name 'R' is not defined in module './m'"
    );
    let source = "include { P as Q; helper } from './m'\nworkflow {\n    Q(helper())\n}";
    let parsed = parse_script(source);
    let module = lower_script(&parsed.tree, source, &mut Stub).expect("lowers");
    let aliases: Vec<&str> = module
        .includes
        .iter()
        .map(|include| &*include.alias)
        .collect();
    assert_eq!(aliases, ["Q", "helper"]);
}
