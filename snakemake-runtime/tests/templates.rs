//! Rules of `template_engine:`, which iunctus renders natively for yte, and directives that
//! iunctus refuses as unsupported: jinja2 templates and `cwl:`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use workflow_graph::Launch;

const TEMPLATE: &str = "name: ?wildcards.sample\nthreshold: ?params.foo\nvalues:\n  ?for i in range(2):\n    - ?i * 2\n?if config[\"flag\"]:\n  flagged: true\n?else:\n  flagged: false\nlabel: plain text\n";

/// A simulated launch of a rule `render` with the action `action`.
fn launch(folder: &std::path::Path, action: &str) -> Launch {
    std::fs::write(folder.join("template.yaml"), TEMPLATE).expect("a template");
    std::fs::write(folder.join("config.yaml"), "flag: true\n").expect("a config");
    let snakefile = format!(
        "configfile: 'config.yaml'\nrule all:\n    input: 'results/a.yaml'\nrule render:\n    input: 'template.yaml'\n    output: 'results/{{sample}}.yaml'\n    params: foo=0.1\n    {action}\n"
    );
    std::fs::write(folder.join("Snakefile"), snakefile).expect("a Snakefile");
    Launch {
        target: folder.to_path_buf(),
        scratch: folder.join("scratch"),
        simulated: true,
        ..Launch::default()
    }
}

#[test]
fn renders_yte_templates() {
    let folder = tempfile::tempdir().expect("a folder");
    let launch = launch(folder.path(), "template_engine: 'yte'");
    let mut driver = snakemake_runtime::driver(&launch).expect("a driver");
    let task = driver.start().expect("starts").remove(0);
    let [_, staged, output] = task.command.as_slice() else {
        panic!("a copy command: {:?}", task.command);
    };
    assert_eq!(output, "results/a.yaml");
    let rendered = (task.inputs.iter())
        .find(|input| &input.path == staged)
        .and_then(|input| input.content.clone())
        .expect("the rendered template");
    // Snakemake 9.27 writes this for the same template.
    let expected = "name: a\nthreshold: 0.1\nvalues:\n- 0\n- 2\nflagged: true\nlabel: plain text\n";
    assert_eq!(rendered, expected);
}

#[test]
fn refuses_unsupported_actions() {
    for action in ["template_engine: 'jinja2'", "cwl: 'tool.cwl'"] {
        let folder = tempfile::tempdir().expect("a folder");
        let launch = launch(folder.path(), action);
        let error = snakemake_runtime::driver(&launch)
            .expect("a driver")
            .start()
            .expect_err("an unsupported action");
        assert!(error.unsupported, "{action}: {error}");
    }
    let folder = tempfile::tempdir().expect("a folder");
    let launch = launch(folder.path(), "template_engine: 'mako'");
    let error = snakemake_runtime::driver(&launch)
        .expect("a driver")
        .start()
        .expect_err("an unknown engine");
    assert!(!error.unsupported, "{error}");
    assert!(
        error.message.contains("Unsupported template engine mako"),
        "{error}"
    );
}
