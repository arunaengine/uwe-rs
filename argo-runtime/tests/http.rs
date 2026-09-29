//! HTTP request contracts and recorded attempt timing at the driver boundary.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use workflow_graph::{Driver, Launch, Outcome, OutputFile, Task};

fn driver(folder: &std::path::Path, spec: &str) -> Box<dyn Driver> {
    let target = folder.join("workflow.yaml");
    std::fs::write(&target, format!(
        "apiVersion: argoproj.io/v1alpha1\nkind: Workflow\nmetadata: {{name: http}}\nspec:\n{spec}"
    )).unwrap();
    argo_runtime::driver(&Launch {
        target,
        ..Launch::default()
    })
    .unwrap()
}

fn response(folder: &std::path::Path, task: &Task, headers: &str) -> Outcome {
    let files = ["@literal\r\n", "200", headers]
        .into_iter()
        .zip(&task.outputs)
        .map(|(text, path)| {
            let location = folder.join(path.trim_start_matches("/tmp/"));
            std::fs::write(&location, text).unwrap();
            OutputFile {
                path: path.clone(),
                location: location.to_string_lossy().into_owned(),
                ..OutputFile::default()
            }
        })
        .collect();
    Outcome {
        id: task.id,
        files,
        ..Outcome::default()
    }
}

#[test]
fn preserves_http_bodies() {
    let folder = tempfile::tempdir().unwrap();
    for (body, prefix, timeout) in [
        (
            "    body: '@literal'\n    timeoutSeconds: 7\n",
            "printf '%s' @literal | ",
            "7",
        ),
        (
            "    bodyFrom: {bytes: QABCDQo=}\n",
            "printf '%b' '\\0100\\0000\\0102\\0015\\0012' | ",
            "30",
        ),
    ] {
        let spec = format!(
            r#"  entrypoint: request
  templates:
  - name: request
    http:
      url: https://example.invalid
      method: POST
      successCondition: 'response.headers["X-Value"] == ["first", "second"] && response.body == "@literal\r\n"'
{}"#,
            body.lines()
                .map(|line| format!("  {line}\n"))
                .collect::<String>()
        );
        let mut run = driver(folder.path(), &spec);
        let tasks = run.start().unwrap();
        assert_eq!(tasks.len(), 1);
        let script = &tasks[0].command[2];
        assert!(script.starts_with(prefix), "{script}");
        assert!(script.contains("--data-binary @-"), "{script}");
        assert!(
            script.contains(&format!("--max-time {timeout}")),
            "{script}"
        );
        assert!(script.contains("--dump-header /tmp/response-headers"));
        let headers = "HTTP/1.1 302 Found\r\nX-Value: discarded\r\n\r\nHTTP/2 200\r\nx-value: first\r\nX-VALUE: second\r\n\r\n";
        assert!(
            run.complete(response(folder.path(), &tasks[0], headers))
                .unwrap()
                .is_empty()
        );
        assert!(run.finish().is_ok());
    }
}

#[test]
fn rejects_invalid_http() {
    let folder = tempfile::tempdir().unwrap();
    for field in [
        "bodyFrom: {path: /body}",
        "bodyFrom: {bytes: !!!}",
        "bodyFrom: {bytes: A}",
        "timeoutSeconds: 0",
        "timeoutSeconds: invalid",
    ] {
        let field = field.replace("bytes: !!!", "bytes: '!!!'");
        let mut run = driver(
            folder.path(),
            &format!(
                "  entrypoint: request\n  templates:\n  - name: request\n    http:\n      url: https://example.invalid\n      {field}\n"
            ),
        );
        assert!(run.start().is_err(), "{field}");
    }
    let mut run = driver(
        folder.path(),
        "  entrypoint: request\n  templates:\n  - name: request\n    http: {url: https://example.invalid}\n",
    );
    let tasks = run.start().unwrap();
    assert!(
        run.complete(Outcome {
            id: tasks[0].id,
            ..Outcome::default()
        })
        .unwrap()
        .is_empty()
    );
    assert!(
        run.finish()
            .unwrap_err()
            .to_string()
            .contains("HTTP response status")
    );
}

#[test]
fn records_scope_times() {
    let folder = tempfile::tempdir().unwrap();
    let spec = r#"  entrypoint: main
  templates:
  - name: main
    steps:
    - - {name: first, template: first}
    - - name: times
        template: times
        arguments:
          parameters:
          - {name: start, value: '{{steps.first.startedAt}}'}
          - {name: finish, value: '{{steps.first.finishedAt}}'}
  - name: first
    container: {image: alpine, command: ['true']}
  - name: times
    inputs: {parameters: [{name: start}, {name: finish}]}
    container: {image: alpine, command: [echo, '{{inputs.parameters.start}}', '{{inputs.parameters.finish}}']}
"#;
    let mut run = driver(folder.path(), spec);
    let tasks = run.start().unwrap();
    let started = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000);
    let next = run
        .complete(Outcome {
            id: tasks[0].id,
            started: Some(started),
            ended: Some(started + std::time::Duration::from_secs(3)),
            ..Outcome::default()
        })
        .unwrap();
    assert_eq!(
        next[0].command,
        ["echo", "1970-01-01T00:16:40Z", "1970-01-01T00:16:43Z"]
    );
}

#[test]
fn hooks_read_results() {
    let folder = tempfile::tempdir().unwrap();
    let spec = r#"  entrypoint: main
  hooks:
    success:
      expression: workflow.status == "Succeeded"
      template: hook
      arguments: {parameters: [{name: status, value: '{{workflow.status}}'}, {name: result, value: done}]}
  templates:
  - name: main
    steps:
    - - name: first
        template: first
        hooks:
          success:
            expression: steps.first.status == "Succeeded"
            template: hook
            arguments: {parameters: [{name: status, value: '{{steps.first.status}}'}, {name: result, value: '{{steps.first.outputs.result}}'}]}
  - name: first
    script: {image: alpine, command: [sh], source: 'echo result'}
  - name: hook
    inputs: {parameters: [{name: status}, {name: result}]}
    container: {image: alpine, command: [echo, '{{inputs.parameters.status}}', '{{inputs.parameters.result}}', '{{workflow.status}}']}
"#;
    let mut run = driver(folder.path(), spec);
    let tasks = run.start().unwrap();
    let next = run
        .complete(Outcome {
            id: tasks[0].id,
            stdout: "result".into(),
            ..Outcome::default()
        })
        .unwrap();
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].command[1..3], ["Succeeded", "result"]);
    let next = run
        .complete(Outcome {
            id: next[0].id,
            ..Outcome::default()
        })
        .unwrap();
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].command, ["echo", "Succeeded", "done", "Succeeded"]);
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
