//! Workflow subprocesses use their requested working directory in their container task.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use workflow_graph::{Launch, Outcome, Writable};

#[test]
fn stages_subprocess_directory() {
    for external in [false, true] {
        let folder = tempfile::tempdir().unwrap();
        let workflow = folder.path().join("workflow");
        let cwd = if external {
            folder.path().join("other")
        } else {
            workflow.join("other")
        };
        std::fs::create_dir_all(&workflow).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        let requested = if external {
            cwd.to_string_lossy().into_owned()
        } else {
            "other".into()
        };
        std::fs::write(
            workflow.join("Snakefile"),
            format!(
                r#"
import subprocess
subprocess.run(["touch", "made.txt"], cwd="{requested}")
rule all:
    input: []
"#
            ),
        )
        .unwrap();
        let mut driver = snakemake_runtime::driver(&Launch {
            target: workflow,
            scratch: folder.path().join("scratch"),
            ..Launch::default()
        })
        .unwrap();
        let command = driver.start().unwrap().remove(0);
        assert_eq!(command.step, "command");
        assert_eq!(command.workdir.as_deref(), Some(cwd.to_str().unwrap()));
        assert_eq!(command.inputs[0].source, cwd.to_string_lossy());
        assert_eq!(command.inputs[0].path, ".");
        assert_eq!(command.inputs[0].writable, Writable::InPlace);
        let jobs = driver
            .complete(Outcome {
                id: command.id,
                ..Outcome::default()
            })
            .unwrap();
        for job in jobs {
            driver
                .complete(Outcome {
                    id: job.id,
                    ..Outcome::default()
                })
                .unwrap();
        }
        driver.finish().unwrap();
    }
}
