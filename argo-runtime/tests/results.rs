//! A real Argo run delivers the output artifacts of its steps to a folder, as
//! `<folder>/<step>/<artifact>`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use workflow_graph::{
    Attempt, Engine, Event, Executor, Launch, Observer, Outcome, OutputFile, Running,
};

const WORKFLOW: &str = r#"
apiVersion: argoproj.io/v1alpha1
kind: Workflow
metadata:
  name: artifacts
spec:
  entrypoint: make
  templates:
  - name: make
    container:
      image: alpine:3.20
      command: [sh, -c, "echo hi > /tmp/hello.txt"]
    outputs:
      artifacts:
      - name: hello
        path: /tmp/hello.txt
"#;

/// Makes each output of a task as a file.
struct Maker(std::path::PathBuf);

impl Executor for Maker {
    fn run(&mut self, attempt: Attempt) -> Running {
        let task = &attempt.task;
        let files = (task.outputs.iter())
            .map(|output| {
                let location = self.0.join(format!("{}.out", task.id));
                std::fs::write(&location, "hi\n").expect("writes");
                OutputFile {
                    path: output.trim_start_matches('/').to_owned(),
                    location: location.to_string_lossy().into_owned(),
                    ..OutputFile::default()
                }
            })
            .collect();
        Running::ended(Ok(Outcome {
            id: task.id,
            files,
            ..Outcome::default()
        }))
    }
}

struct Targets(Vec<std::path::PathBuf>);

impl Observer for Targets {
    fn event(&mut self, event: Event<'_>) {
        if let Event::Delivered { delivery } = event {
            self.0.push(delivery.target.clone().into());
        }
    }
}

#[test]
fn output_artifacts_go_to_the_results_folder() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let file = folder.path().join("workflow.yaml");
    std::fs::write(&file, WORKFLOW).expect("writes the workflow");
    let launch = Launch {
        target: file,
        scratch: folder.path().join("scratch"),
        output_dir: Some(folder.path().join("results")),
        ..Launch::default()
    };
    let mut driver = argo_runtime::driver(&launch).expect("a driver");
    let mut targets = Targets(Vec::new());
    let execution = Engine::new(&mut Maker(folder.path().to_owned()))
        .observer(&mut targets)
        .run("workflow.yaml", driver.as_mut());
    assert!(execution.error.is_none(), "{:?}", execution.error);
    assert_eq!(targets.0, [folder.path().join("results/artifacts/hello")]);
}
