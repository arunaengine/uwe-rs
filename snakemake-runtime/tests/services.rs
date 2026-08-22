//! Service jobs, as Snakemake 9.27.0 runs `test_service_jobs`: the job with a `service()`
//! output runs at once with the jobs that use it, which end it; the run goes on after them.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use workflow_graph::{Attempt, Engine, Executor, Launch, Outcome, OutputFile, Running, Task};

const SNAKEFILE: &str = r#"
rule all:
    input: "test.txt", "test2.txt", "both.txt"

rule a:
    output: service("foo.socket")
    shell: "ln -s /dev/random {output}; sleep 10000"

rule b:
    input: "foo.socket"
    output: "test.txt"
    shell: "head -n1 {input} > {output}"

rule c:
    input: "foo.socket"
    output: "test2.txt"
    shell: "head -n1 {input} > {output}"

rule d:
    input: "test.txt", "test2.txt"
    output: "both.txt"
    shell: "cat {input} > {output}"
"#;

/// Makes the outputs of each task in a folder of its own, and keeps the tasks.
struct Made {
    folder: std::path::PathBuf,
    tasks: Vec<Task>,
}

impl Executor for Made {
    fn run(&mut self, attempt: Attempt) -> Running {
        let task = attempt.task.clone();
        let mut files = Vec::new();
        for output in &task.outputs {
            let location = self.folder.join(format!("{}-{output}", task.id));
            std::fs::write(&location, "x\n").expect("writes");
            files.push(OutputFile {
                path: output.clone(),
                location: location.to_string_lossy().into_owned(),
                ..OutputFile::default()
            });
        }
        self.tasks.push(Task::clone(&task));
        Running::ended(Ok(Outcome {
            id: task.id,
            files,
            ..Outcome::default()
        }))
    }
}

#[test]
fn a_service_runs_with_the_jobs_that_use_it_in_one_task() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let workdir = folder.path().join("workflow");
    std::fs::create_dir(&workdir).expect("makes the folder");
    std::fs::write(workdir.join("Snakefile"), SNAKEFILE).expect("writes the Snakefile");
    let launch = Launch {
        target: workdir,
        scratch: folder.path().join("scratch"),
        ..Launch::default()
    };
    let mut driver = snakemake_runtime::driver(&launch).expect("a driver");
    let made = folder.path().join("made");
    std::fs::create_dir(&made).expect("makes the folder");
    let mut executor = Made {
        folder: made,
        tasks: Vec::new(),
    };
    let execution = Engine::new(&mut executor).run("Snakefile", driver.as_mut());
    assert!(execution.error.is_none(), "{:?}", execution.error);
    let steps: Vec<&str> = executor
        .tasks
        .iter()
        .map(|task| task.step.as_str())
        .collect();
    assert_eq!(steps, ["a", "d", "all"]);
    let group = &executor.tasks[0];
    assert_eq!(group.outputs, ["test.txt", "test2.txt"]);
    assert!(group.inputs.iter().all(|input| input.path != "foo.socket"));
    let script = group.script.as_deref().unwrap_or_default();
    assert!(script.contains("kill -TERM \"$service\""), "{script}");
}
