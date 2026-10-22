//! Real-mode loop and output handling through the driver completion boundary.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use workflow_graph::{Driver, Launch, Outcome};

fn driver(folder: &std::path::Path, spec: &str, simulated: bool) -> Box<dyn Driver> {
    let target = folder.join("workflow.yaml");
    std::fs::write(&target, format!(
        "apiVersion: argoproj.io/v1alpha1\nkind: Workflow\nmetadata: {{name: loops}}\nspec:\n{spec}"
    )).unwrap();
    argo_runtime::driver(&Launch {
        target,
        simulated,
        ..Launch::default()
    })
    .unwrap()
}

fn fanout(dag: bool) -> String {
    let calls = if dag {
        r#"    dag:
      tasks:
      - {name: produce, template: produce}
      - name: use
        dependencies: [produce]
        template: use
        withParam: "{{tasks.produce.outputs.result}}"
        arguments: {parameters: [{name: item, value: "{{item}}"}]}
"#
    } else {
        r#"    steps:
    - - {name: produce, template: produce}
    - - name: use
        template: use
        withParam: "{{steps.produce.outputs.result}}"
        arguments: {parameters: [{name: item, value: "{{item}}"}]}
"#
    };
    format!(
        r#"  entrypoint: main
  templates:
  - name: main
{calls}  - name: produce
    script: {{image: alpine, command: [sh], source: 'echo items'}}
  - name: use
    inputs: {{parameters: [{{name: item}}]}}
    container: {{image: alpine, command: [echo, "{{{{inputs.parameters.item}}}}"]}}
"#
    )
}

#[test]
fn rejects_unknown_fanout() {
    let folder = tempfile::tempdir().unwrap();
    for dag in [false, true] {
        for text in ["  ", "{{missing}}", "placeholder_items"] {
            let mut run = driver(folder.path(), &fanout(dag), false);
            let tasks = run.start().unwrap();
            assert_eq!(tasks.len(), 1);
            assert!(
                run.complete(Outcome {
                    id: tasks[0].id,
                    stdout: text.into(),
                    ..Outcome::default()
                })
                .unwrap()
                .is_empty()
            );
            assert!(run.finish().unwrap_err().to_string().contains("withParam"));
        }
        for (text, simulated) in [("[]", false), ("  ", true), ("{{missing}}", true)] {
            let mut run = driver(folder.path(), &fanout(dag), simulated);
            let tasks = run.start().unwrap();
            assert!(
                run.complete(Outcome {
                    id: tasks[0].id,
                    stdout: text.into(),
                    ..Outcome::default()
                })
                .unwrap()
                .is_empty()
            );
            assert!(run.finish().is_ok());
        }
    }
}

#[test]
fn empty_preserves_validation() {
    let folder = tempfile::tempdir().unwrap();
    let spec = fanout(false).replace("{{item}}", "{{item.missing}}");
    for simulated in [false, true] {
        let mut run = driver(folder.path(), &spec, simulated);
        let tasks = run.start().unwrap();
        let next = run
            .complete(Outcome {
                id: tasks[0].id,
                stdout: "[\"\"]".into(),
                ..Outcome::default()
            })
            .unwrap();
        if simulated {
            assert_eq!(next.len(), 1);
        } else {
            assert!(next.is_empty());
            assert!(
                run.finish()
                    .unwrap_err()
                    .to_string()
                    .contains("item.missing")
            );
        }
    }
}

#[test]
fn preserves_skipped_references() {
    let folder = tempfile::tempdir().unwrap();
    for (calls, source) in [
        (
            r#"    steps:
    - - {name: produce, template: produce, when: 'false'}
    - - {name: use, template: produce, when: 'false', withParam: '{{missing}}'}
"#,
            "steps.produce.outputs.result",
        ),
        (
            r#"    dag:
      tasks:
      - {name: anchor, template: produce, when: 'false'}
      - {name: produce, template: produce, depends: 'anchor.Failed'}
      - {name: use, template: produce, depends: 'produce.Succeeded', withParam: '{{missing}}'}
"#,
            "tasks.produce.outputs.result",
        ),
    ] {
        let spec = format!(
            r#"  entrypoint: main
  templates:
  - name: main
{calls}    outputs:
      parameters:
      - name: result
        valueFrom: {{expression: '{source}'}}
  - name: produce
    script: {{image: alpine, command: [sh], source: 'echo items'}}
"#
        );
        let mut run = driver(folder.path(), &spec, false);
        assert!(run.start().unwrap().is_empty());
        assert!(run.finish().is_ok());
    }
}

#[test]
fn propagates_empty_errors() {
    let folder = tempfile::tempdir().unwrap();
    let spec = r#"  entrypoint: main
  templates:
  - name: main
    steps:
    - - {name: produce, template: produce}
    outputs:
      parameters:
      - name: result
        valueFrom: {expression: "jsonpath(steps.produce.outputs.result, '$.missing')"}
  - name: produce
    script: {image: alpine, command: [sh], source: 'echo items'}
"#;
    for simulated in [false, true] {
        let mut run = driver(folder.path(), spec, simulated);
        let tasks = run.start().unwrap();
        assert!(
            run.complete(Outcome {
                id: tasks[0].id,
                ..Outcome::default()
            })
            .unwrap()
            .is_empty()
        );
        assert_eq!(run.finish().is_ok(), simulated);
    }
}

#[test]
fn preserves_aggregate_strings() {
    let folder = tempfile::tempdir().unwrap();
    for (calls, prefix) in [
        (
            r#"    steps:
    - - name: make
        template: make
        withItems: ['42', 'null', '{"a":1}', '"quoted"']
        arguments: {parameters: [{name: text, value: '{{item}}'}]}
    - - {name: consume, template: consume, arguments: {parameters: [{name: list, value: '{{steps.make.outputs.parameters.text}}'}, {name: maps, value: '{{steps.make.outputs.parameters}}'}, {name: results, value: '{{steps.make.outputs.result}}'}]}}
"#,
            "steps",
        ),
        (
            r#"    dag:
      tasks:
      - name: make
        template: make
        withItems: ['42', 'null', '{"a":1}', '"quoted"']
        arguments: {parameters: [{name: text, value: '{{item}}'}]}
      - {name: consume, template: consume, dependencies: [make], arguments: {parameters: [{name: list, value: '{{tasks.make.outputs.parameters.text}}'}, {name: maps, value: '{{tasks.make.outputs.parameters}}'}, {name: results, value: '{{tasks.make.outputs.result}}'}]}}
"#,
            "tasks",
        ),
    ] {
        let spec = format!(
            r#"  entrypoint: main
  templates:
  - name: main
{calls}  - name: make
    inputs: {{parameters: [{{name: text}}]}}
    script: {{image: alpine, command: [sh], source: 'echo items'}}
    outputs: {{parameters: [{{name: text, value: '{{{{inputs.parameters.text}}}}'}}]}}
  - name: consume
    inputs: {{parameters: [{{name: list}}, {{name: maps}}, {{name: results}}]}}
    container: {{image: alpine, command: [echo, '{{{{inputs.parameters.list}}}}', '{{{{inputs.parameters.maps}}}}', '{{{{inputs.parameters.results}}}}']}}
"#
        );
        let mut run = driver(folder.path(), &spec, false);
        let tasks = run.start().unwrap();
        assert_eq!(tasks.len(), 4, "{prefix}");
        let mut next = Vec::new();
        for task in tasks.into_iter().rev() {
            next.extend(
                run.complete(Outcome {
                    id: task.id,
                    stdout: "42".into(),
                    ..Outcome::default()
                })
                .unwrap(),
            );
        }
        assert_eq!(next.len(), 1);
        assert_eq!(
            next[0].command[1],
            r#"["42","null","{\"a\":1}","\"quoted\""]"#
        );
        assert_eq!(
            next[0].command[2],
            r#"[{"text":"42"},{"text":"null"},{"text":"{\"a\":1}"},{"text":"\"quoted\""}]"#
        );
        assert_eq!(next[0].command[3], "[42,42,42,42]");
        assert!(
            run.complete(Outcome {
                id: next[0].id,
                ..Outcome::default()
            })
            .unwrap()
            .is_empty()
        );
        assert!(run.finish().is_ok());
    }
}

#[test]
fn bounds_sequence_expansion() {
    let folder = tempfile::tempdir().unwrap();
    for sequence in [
        "{count: '1000000000000'}",
        "{start: '-9223372036854775808', end: '9223372036854775807'}",
        "{count: 1, format: '%999999999999999999999999d'}",
    ] {
        let spec = format!(
            "  entrypoint: main\n  parallelism: 1\n  templates:\n  - name: main\n    steps:\n    - - name: work\n        template: leaf\n        withSequence: {sequence}\n  - name: leaf\n    container: {{image: alpine}}\n"
        );
        let mut run = driver(folder.path(), &spec, false);
        let tasks = run.start().unwrap();
        assert!(tasks.is_empty());
        assert!(run.finish().unwrap_err().message.contains("limit"));
    }
}
