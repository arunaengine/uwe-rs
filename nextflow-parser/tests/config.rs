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
        ConfigStatement::If { .. } => "if".to_owned(),
        ConfigStatement::Try { .. } => "try".to_owned(),
        ConfigStatement::Declaration { names, .. } => {
            let names: Vec<_> = names.iter().map(|name| name.text.as_str()).collect();
            format!("def {}", names.join(", "))
        }
        ConfigStatement::Function(function) => format!("{}()", function.name.text),
        ConfigStatement::Statement(_) => "statement".to_owned(),
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

/// The messages of the legacy syntax in a file, which strict syntax rejects.
fn legacy(source: &str) -> Vec<String> {
    let parsed = parse_config(source);
    assert!(
        parsed.errors.is_empty(),
        "unexpected errors: {:?}",
        parsed.errors
    );
    parsed
        .legacy
        .into_iter()
        .map(|error| error.message)
        .collect()
}

#[test]
fn legacy_statements() {
    let source = "if (params.gpu) {
    docker.runOptions = '--gpus all'
}
def prefix = 'x'
String name = 'y'
try {
    foo = 1
} catch (Exception e) {
}
";
    assert_eq!(
        legacy(source),
        [
            "If statements cannot be mixed with config statements",
            "Variable declarations cannot be mixed with config statements",
            "Variable declarations cannot be mixed with config statements",
            "Try-catch blocks cannot be mixed with config statements"
        ]
    );
    assert_eq!(
        labels(&parse(source)),
        ["if", "def prefix", "def name", "try"]
    );
}

#[test]
fn legacy_if() {
    let statements = parse(
        "process {
    if (params.gpu) {
        withName: 'FOO' {
            accelerator = 1
        }
    } else if (params.big)
        cpus = 8
    else {
        includeConfig 'x.config'
    }
}
",
    );
    let [
        ConfigStatement::If {
            then, otherwise, ..
        },
    ] = body(&statements[0])
    else {
        panic!("expected an if statement: {statements:?}");
    };
    assert_eq!(labels(then), ["withName: FOO"]);
    let [
        ConfigStatement::If {
            then, otherwise, ..
        },
    ] = otherwise.as_slice()
    else {
        panic!("expected else if: {otherwise:?}");
    };
    assert_eq!(labels(then), ["cpus"]);
    assert_eq!(labels(otherwise), ["includeConfig"]);
}

#[test]
fn legacy_try() {
    let statements = parse(
        "try {
    includeConfig \"${params.custom_config_base}/nfcore_custom.config\"
} catch (Exception e) {
    System.err.println(\"WARNING: Could not load nf-core/config profiles\")
}
",
    );
    let [ConfigStatement::Try { body, catches, .. }] = statements.as_slice() else {
        panic!("expected a try statement: {statements:?}");
    };
    assert_eq!(labels(body), ["includeConfig"]);
    assert_eq!(catches.len(), 1);
    assert_eq!(catches[0].name.text, "e");
    assert_eq!(catches[0].types[0].parts[0].text, "Exception");
    assert_eq!(labels(&catches[0].body), ["statement"]);
}

#[test]
fn legacy_functions() {
    let source = "def check_max(obj, type) {
    if (type == 'cpus') {
        return Math.min(obj, params.max_cpus as int)
    }
}
String upper(String text) {
    text.toUpperCase()
}
";
    assert_eq!(
        legacy(source),
        ["Unexpected input: '('", "Unexpected input: '('"]
    );
    assert_eq!(labels(&parse(source)), ["check_max()", "upper()"]);
}

#[test]
fn legacy_script_statements() {
    let source = "println 'loading'\nlog.info(\"x\")\n";
    assert_eq!(
        legacy(source),
        ["Unexpected input: ''loading''", "Unexpected input: '('"]
    );
    assert_eq!(labels(&parse(source)), ["statement", "statement"]);
    // What is no script statement either keeps the strict error.
    assert_eq!(messages("foo = )\n"), ["Expected an expression"]);
    assert_eq!(messages("a.b c )\n"), ["Unexpected input: 'c'"]);
}

#[test]
fn rejects_script_types() {
    let sources = [
        "x = { def a: String = y }\n",
        "x = { def (a: String, b) = y }\n",
        "x = { def (a,\n  b) = y }\n",
        "x = { v: String -> v }\n",
        "x = { try { a } catch (e: Exception) { b } }\n",
        "x = { a as String? }\n",
        "x = { a as List<?> }\n",
    ];
    for source in sources {
        assert!(!messages(source).is_empty(), "{source}");
    }
    let legacy = "x = { String v -> def (a, b) = v as List<String> }\n";
    assert!(messages(legacy).is_empty());
}

#[test]
fn rejects_closure_labels() {
    assert!(!messages("process.ext.args = x.branch { big: 1 }\n").is_empty());
    assert!(messages("process.ext.args = x.collect { it }\n").is_empty());
}

#[test]
fn incomplete_statement() {
    assert_eq!(messages("process.cpus\n"), ["Incomplete statement"]);
    assert_eq!(messages("process.\n"), ["Incomplete statement"]);
}

#[test]
fn legacy_top_selector() {
    let source = "withName: FOO {\n    cpus = 2\n}\n";
    assert_eq!(legacy(source), ["Unexpected input: ':'"]);
    assert_eq!(labels(&parse(source)), ["withName: FOO"]);
}

#[test]
fn legacy_labeled_statement() {
    let source = "process {\n    withName: FOO {\n        saveAs: { f -> f }\n    }\n}\n";
    assert_eq!(legacy(source), ["Expected a name"]);
    let statements = parse(source);
    let selector = &body(&statements[0])[0];
    assert_eq!(labels(body(selector)), ["statement"]);
}

#[test]
fn recovers_after_error() {
    let parsed = parse_config("foo = )\nprocess.cpus = 2\nparams {\n    outdir = 'results'\n}\n");
    assert_eq!(parsed.errors.len(), 1, "errors: {:?}", parsed.errors);
    assert_eq!(labels(&parsed.tree.statements), ["process.cpus", "params"]);
}
