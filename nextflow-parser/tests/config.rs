//! Tests how config files parse into assignments, blocks, selectors, plugin blocks and includes.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_parser::ast::ConfigStatement;
use nextflow_parser::parse_config;

/// Parses a config file that must have no errors.
fn parse(source: &str) -> Vec<ConfigStatement> {
    let parsed = parse_config(source);
    assert!(
        parsed.errors.is_empty(),
        "unexpected errors: {:?}",
        parsed.errors
    );
    parsed.tree.statements
}

fn messages(source: &str) -> Vec<String> {
    let parsed = parse_config(source);
    parsed
        .errors
        .into_iter()
        .map(|error| error.message)
        .collect()
}

/// A short description of a statement: the path of an assignment, the name of a block, or the
/// kind and target of a selector.
fn label(statement: &ConfigStatement) -> String {
    match statement {
        ConfigStatement::Include { .. } => "includeConfig".to_owned(),
        ConfigStatement::Assign { path, .. } => {
            let parts: Vec<_> = path.iter().map(|name| name.text.as_str()).collect();
            parts.join(".")
        }
        ConfigStatement::Block { name, .. } | ConfigStatement::Apply { name, .. } => {
            name.text.clone()
        }
        ConfigStatement::Selector { kind, target, .. } => {
            format!("{}: {}", kind.text, target.text)
        }
    }
}

fn labels(statements: &[ConfigStatement]) -> Vec<String> {
    statements.iter().map(label).collect()
}

/// The statements inside a block or selector.
fn body(statement: &ConfigStatement) -> &[ConfigStatement] {
    match statement {
        ConfigStatement::Block { body, .. } | ConfigStatement::Selector { body, .. } => body,
        other => panic!("expected a block, found {other:?}"),
    }
}

#[test]
fn assignments() {
    let statements = parse(
        "process.executor = 'slurm'
docker.runOptions = '-u $(id -u):$(id -g)'
nextflow.enable.configProcessNamesValidation = false

// Set bash options
process.shell = [
    \"bash\",
    \"-C\",         // No clobber - prevent output redirection from overwriting files.
    \"-e\",         // Exit if a tool returns a non-zero status/exit code
]
",
    );
    assert_eq!(
        labels(&statements),
        [
            "process.executor",
            "docker.runOptions",
            "nextflow.enable.configProcessNamesValidation",
            "process.shell"
        ]
    );
}

#[test]
fn nested_blocks() {
    let statements = parse(
        "params {
    // Input options
    input                      = null
    outdir                     = null
}

env {
    PYTHONNOUSERSITE = 1
    R_PROFILE_USER   = \"/.Rprofile\"
}

profiles {
    docker {
        docker.enabled          = true
        conda.enabled           = false
    }
    test      { includeConfig 'conf/test.config'      }
}
",
    );
    assert_eq!(labels(&statements), ["params", "env", "profiles"]);
    assert_eq!(labels(body(&statements[0])), ["input", "outdir"]);
    assert_eq!(
        labels(body(&statements[1])),
        ["PYTHONNOUSERSITE", "R_PROFILE_USER"]
    );
    let profiles = body(&statements[2]);
    assert_eq!(labels(profiles), ["docker", "test"]);
    assert_eq!(
        labels(body(&profiles[0])),
        ["docker.enabled", "conda.enabled"]
    );
    assert_eq!(labels(body(&profiles[1])), ["includeConfig"]);
}

#[test]
fn process_selectors() {
    let statements = parse(
        "process {
    publishDir = [
        path: { \"${params.outdir}/${task.process.tokenize(':')[-1].tokenize('_')[0].toLowerCase()}\" },
        mode: params.publish_dir_mode,
    ]

    withName: FASTQC_PRE_CONVERSION {
        ext.args   = '--quiet'
        ext.prefix = { \"${meta.id}.pre_conversion\" }
    }

    withLabel: 'process_high' {
        cpus   = { 12    * task.attempt }
        memory = { 72.GB * task.attempt }
    }
}
",
    );
    let process = body(&statements[0]);
    assert_eq!(
        labels(process),
        [
            "publishDir",
            "withName: FASTQC_PRE_CONVERSION",
            "withLabel: process_high"
        ]
    );
    assert_eq!(labels(body(&process[1])), ["ext.args", "ext.prefix"]);
    assert_eq!(labels(body(&process[2])), ["cpus", "memory"]);
}

#[test]
fn plugin_block() {
    let statements = parse(
        "plugins {
    id 'nf-schema@2.6.1'     // Validation of pipeline parameters
    id 'nf-prov'
}
",
    );
    let [ConfigStatement::Apply { name, calls, .. }] = &statements[..] else {
        panic!("expected one plugin block, found {statements:?}");
    };
    assert_eq!(name.text, "plugins");
    assert_eq!(calls.len(), 2);
    assert!(calls.iter().all(|call| call.name.text == "id"));
    assert!(calls.iter().all(|call| call.arguments.len() == 1));
}

#[test]
fn rejects_apply_block() {
    assert_eq!(
        messages("process {\n    cpus 2\n}\n"),
        ["Config settings must be assigned with an equals sign (`=`)"]
    );
}

#[test]
fn include_expressions() {
    let statements = parse(
        "includeConfig 'conf/base.config'
includeConfig !params.igenomes_ignore ? 'conf/igenomes.config' : 'conf/igenomes_ignored.config'
",
    );
    assert_eq!(labels(&statements), ["includeConfig", "includeConfig"]);
}

#[test]
fn rejects_script_statements() {
    let parsed = parse_config(
        "if (params.gpu) {
    docker.runOptions = '--gpus all'
}
def prefix = 'x'
String name = 'y'
try {
    foo = 1
} catch (Exception e) {
}
",
    );
    assert!(parsed.tree.statements.is_empty());
    let messages: Vec<_> = parsed.errors.iter().map(|error| &*error.message).collect();
    assert_eq!(
        messages,
        [
            "If statements cannot be mixed with config statements",
            "Variable declarations cannot be mixed with config statements",
            "Variable declarations cannot be mixed with config statements",
            "Try-catch blocks cannot be mixed with config statements"
        ]
    );
}

#[test]
fn incomplete_statement() {
    assert_eq!(messages("process.cpus\n"), ["Incomplete statement"]);
    assert_eq!(messages("process.\n"), ["Incomplete statement"]);
}

#[test]
fn rejects_top_selector() {
    let source = "withName: FOO {\n    cpus = 2\n}\n";
    assert_eq!(messages(source), ["Unexpected input: ':'"]);
}

#[test]
fn recovers_after_error() {
    let parsed = parse_config("foo bar\nprocess.cpus = 2\nparams {\n    outdir = 'results'\n}\n");
    assert_eq!(parsed.errors.len(), 1, "errors: {:?}", parsed.errors);
    assert_eq!(labels(&parsed.tree.statements), ["process.cpus", "params"]);
}
