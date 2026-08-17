//! What a real Snakemake run leaves in the working directory: the outputs of its jobs, but not
//! the temporary ones; and a failed job runs again while its rule's `retries` allow, with its
//! resources evaluated for the next attempt.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use workflow_graph::{
    Attempt, Engine, Event, Executor, Launch, Observer, Outcome, OutputFile, Running,
};

const SNAKEFILE: &str = r#"
rule all:
    input: "final.txt"

rule make:
    output: temp("part.txt")
    retries: 1
    resources: mem_mb=lambda wildcards, attempt: attempt * 100
    shell: "echo part > {output}"

rule finish:
    input: "part.txt"
    output: "final.txt"
    shell: "cat {input} > {output}"
"#;

/// Fails the first attempt of `make`, makes each output in a folder of its own, and notes the
/// rule and memory of each attempt.
struct Flaky {
    folder: std::path::PathBuf,
    ran: Vec<(String, u32, Option<u64>)>,
}

impl Executor for Flaky {
    fn run(&mut self, attempt: Attempt) -> Running {
        let task = &attempt.task;
        // Snakemake's megabytes have 10^6 bytes.
        let memory = task.resources.memory_bytes.map(|bytes| bytes / 1_000_000);
        self.ran.push((task.step.clone(), attempt.number, memory));
        if task.step == "make" && attempt.number == 1 {
            return Running::ended(Ok(Outcome {
                id: task.id,
                exit_code: 1,
                ..Outcome::default()
            }));
        }
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
        Running::ended(Ok(Outcome {
            id: task.id,
            files,
            ..Outcome::default()
        }))
    }
}

/// Notes the targets of the deliveries, relative to the working directory.
struct Targets(Vec<String>, std::path::PathBuf);

impl Observer for Targets {
    fn event(&mut self, event: Event<'_>) {
        if let Event::Delivered { delivery } = event {
            let target = std::path::Path::new(&delivery.target);
            let target = target.strip_prefix(&self.1).unwrap_or(target);
            self.0.push(target.display().to_string());
        }
    }
}

#[test]
fn outputs_go_to_the_working_directory_and_failed_jobs_run_again() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let workdir = folder.path().join("workflow");
    std::fs::create_dir(&workdir).expect("makes the folder");
    std::fs::write(workdir.join("Snakefile"), SNAKEFILE).expect("writes the Snakefile");
    let launch = Launch {
        target: workdir.clone(),
        scratch: folder.path().join("scratch"),
        ..Launch::default()
    };
    let mut driver = snakemake_runtime::driver(&launch).expect("a driver");
    let made = folder.path().join("made");
    std::fs::create_dir(&made).expect("makes the folder");
    let mut executor = Flaky {
        folder: made,
        ran: Vec::new(),
    };
    let mut targets = Targets(
        Vec::new(),
        workdir.canonicalize().unwrap_or(workdir.clone()),
    );
    let execution = Engine::new(&mut executor)
        .observer(&mut targets)
        .run("Snakefile", driver.as_mut());
    assert!(execution.error.is_none(), "{:?}", execution.error);
    let ran: Vec<(&str, u32, Option<u64>)> = (executor.ran.iter())
        .map(|(rule, attempt, memory)| (rule.as_str(), *attempt, *memory))
        .collect();
    // Rules without resources get Snakemake's default of 1000 MB.
    let expected = [
        ("make", 1, Some(100)),
        ("make", 2, Some(200)),
        ("finish", 1, Some(1000)),
        ("all", 1, Some(1000)),
    ];
    assert_eq!(ran, expected);
    // The temporary output of `make` stays out of the working directory.
    assert_eq!(targets.0, ["final.txt"]);
    assert!(Path::new(&targets.1).is_dir());
}

/// `outputflags: temp` makes every output temporary, unless the rule marks it `protected`; a
/// pipe is temporary anyway.
const DEFAULT_FLAGS: &str = r#"
outputflags:
    temp

rule all:
    input: "final.txt", "kept.txt"

rule produce:
    output: "part.txt"
    shell: "echo part > {output}"

rule stream:
    output: pipe("stream.txt")
    shell: "echo stream > {output}"

rule keep:
    output: protected("kept.txt")
    shell: "echo kept > {output}"

rule finish:
    input: "part.txt", "stream.txt"
    output: "final.txt"
    shell: "cat {input} > {output}"
"#;

#[test]
fn default_output_flags_make_outputs_temporary() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let workdir = folder.path().join("workflow");
    std::fs::create_dir(&workdir).expect("makes the folder");
    std::fs::write(workdir.join("Snakefile"), DEFAULT_FLAGS).expect("writes the Snakefile");
    let launch = Launch {
        target: workdir.clone(),
        scratch: folder.path().join("scratch"),
        ..Launch::default()
    };
    let mut driver = snakemake_runtime::driver(&launch).expect("a driver");
    let made = folder.path().join("made");
    std::fs::create_dir(&made).expect("makes the folder");
    let mut executor = Flaky {
        folder: made,
        ran: Vec::new(),
    };
    let mut targets = Targets(
        Vec::new(),
        workdir.canonicalize().unwrap_or(workdir.clone()),
    );
    let execution = Engine::new(&mut executor)
        .observer(&mut targets)
        .run("Snakefile", driver.as_mut());
    assert!(execution.error.is_none(), "{:?}", execution.error);
    // Snakemake 9.27 removes the temporary `part.txt` and the pipe `stream.txt`, and keeps the
    // inputs of the target rule.
    targets.0.sort();
    assert_eq!(targets.0, ["final.txt", "kept.txt"]);
}
