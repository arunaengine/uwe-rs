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

#[test]
fn ensured_outputs_are_checked_before_delivery_and_retried() {
    for ensure in [
        "non_empty=True",
        "sha256=lambda wildcards: 'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad'",
        "md5='900150983cd24fb0d6963f7d28e17f72'",
        "sha1='a9993e364706816aba3e25717850c26c9cd0d89d'",
    ] {
        let folder = tempfile::tempdir().unwrap();
        let snakefile = format!(
            r#"
rule make:
    output: "good.txt", ensure("checked.txt", {ensure})
    retries: 1
    resources: mem_mb=lambda wildcards, attempt: attempt * 100
    shell: "true"
"#
        );
        std::fs::write(folder.path().join("Snakefile"), snakefile).unwrap();
        let launch = Launch {
            target: folder.path().to_path_buf(),
            scratch: folder.path().join("scratch"),
            ..Launch::default()
        };
        let mut driver = snakemake_runtime::driver(&launch).unwrap();
        let first = driver.start().unwrap().remove(0);
        assert_eq!(first.resources.memory_bytes, Some(96 * 1024 * 1024));
        let good = folder.path().join("good");
        let checked = folder.path().join("checked");
        std::fs::write(&good, "good").unwrap();
        std::fs::write(&checked, "").unwrap();
        let outcome = || Outcome {
            id: first.id,
            files: vec![
                OutputFile {
                    path: "good.txt".into(),
                    location: good.to_string_lossy().into_owned(),
                    ..OutputFile::default()
                },
                OutputFile {
                    path: "checked.txt".into(),
                    location: checked.to_string_lossy().into_owned(),
                    ..OutputFile::default()
                },
            ],
            ..Outcome::default()
        };
        let retry = driver.complete(outcome()).unwrap().remove(0);
        assert_eq!(retry.id, first.id);
        assert_eq!(retry.resources.memory_bytes, Some(191 * 1024 * 1024));
        assert!(driver.deliveries().is_empty(), "{ensure}");
        std::fs::write(&checked, "abc").unwrap();
        assert!(driver.complete(outcome()).unwrap().is_empty());
        assert_eq!(driver.deliveries().len(), 2);
        driver.finish().unwrap();
    }
}

#[test]
fn missing_outputs_retry_and_exhausted_integrity_failures_deliver_nothing() {
    for output in ["\"out.txt\"", "ensure(\"out.txt\", non_empty=True)"] {
        let folder = tempfile::tempdir().unwrap();
        std::fs::write(
            folder.path().join("Snakefile"),
            format!(
                r#"
rule make:
    output: {output}
    retries: 1
    shell: "true"
"#
            ),
        )
        .unwrap();
        let launch = Launch {
            target: folder.path().to_path_buf(),
            scratch: folder.path().join("scratch"),
            ..Launch::default()
        };
        let mut driver = snakemake_runtime::driver(&launch).unwrap();
        let first = driver.start().unwrap().remove(0);
        let made = folder.path().join("empty");
        std::fs::write(&made, "").unwrap();
        let outcome = || Outcome {
            id: first.id,
            files: if output.starts_with("ensure") {
                vec![OutputFile {
                    path: "out.txt".into(),
                    location: made.to_string_lossy().into_owned(),
                    ..OutputFile::default()
                }]
            } else {
                Vec::new()
            },
            ..Outcome::default()
        };
        assert_eq!(driver.complete(outcome()).unwrap().len(), 1);
        assert!(driver.complete(outcome()).unwrap().is_empty());
        assert!(driver.deliveries().is_empty());
        assert!(driver.finish().is_err());
    }
}

#[test]
fn storage_outputs_deliver_to_their_address_and_consumers_use_the_new_bytes() {
    use std::path::PathBuf;
    use workflow_graph::{Delivery, Files, LocalFiles};

    struct Bucket(PathBuf);
    impl Files for Bucket {
        fn local(&self, location: &str) -> std::io::Result<PathBuf> {
            Ok(location
                .strip_prefix("s3://bucket/")
                .map_or_else(|| PathBuf::from(location), |key| self.0.join(key)))
        }

        fn deliver(&self, delivery: &Delivery) -> std::io::Result<()> {
            LocalFiles.deliver(&Delivery {
                target: self.local(&delivery.target)?.to_string_lossy().into_owned(),
                ..delivery.clone()
            })
        }
    }

    let folder = tempfile::tempdir().expect("a temporary directory");
    let workdir = folder.path().join("workflow");
    let bucket = std::sync::Arc::new(Bucket(folder.path().join("bucket")));
    std::fs::create_dir_all(&workdir).expect("makes the workflow folder");
    std::fs::create_dir_all(&bucket.0).expect("makes the bucket");
    std::fs::write(bucket.0.join("A.txt"), "old\n").expect("writes old remote bytes");
    std::fs::write(
        workdir.join("Snakefile"),
        r#"
rule all:
    input: "final.txt"
rule produce:
    output: storage("s3://bucket/{sample}.txt")
    shell: "echo new > {output}"
rule consume:
    input: storage("s3://bucket/A.txt")
    output: "final.txt"
    shell: "cat {input} > {output}"
"#,
    )
    .expect("writes the Snakefile");
    let launch = Launch {
        target: workdir,
        scratch: folder.path().join("scratch"),
        ..Launch::default()
    };
    let mut driver = snakemake_runtime::driver(&launch).expect("a driver");
    driver.use_files(bucket.clone());
    let first = driver.start().expect("starts");
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].step, "produce");
    let made = folder.path().join("made.txt");
    std::fs::write(&made, "new\n").expect("writes producer bytes");
    let next = driver
        .complete(Outcome {
            id: first[0].id,
            files: vec![OutputFile {
                path: first[0].outputs[0].clone(),
                location: made.to_string_lossy().into_owned(),
                ..OutputFile::default()
            }],
            ..Outcome::default()
        })
        .expect("accepts producer output");
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].step, "consume");
    let input = next[0]
        .inputs
        .iter()
        .find(|input| input.path.ends_with("/A.txt"))
        .expect("storage input");
    assert_eq!(
        std::fs::read(&input.source).expect("reads consumer bytes"),
        b"new\n"
    );
    let deliveries = driver.deliveries();
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].target, "s3://bucket/A.txt");
    bucket
        .deliver(&deliveries[0])
        .expect("publishes producer bytes");
    assert_eq!(
        std::fs::read(bucket.0.join("A.txt")).expect("reads remote bytes"),
        b"new\n"
    );
}

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
        simulated: true,
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

/// Makes the outputs of each task in a folder of its own, but no log files.
struct Quiet(std::path::PathBuf);

impl Executor for Quiet {
    fn run(&mut self, attempt: Attempt) -> Running {
        let task = &attempt.task;
        let mut files = Vec::new();
        for output in task
            .outputs
            .iter()
            .filter(|output| !output.starts_with("logs/"))
        {
            let location = self.0.join(format!("{}-{output}", task.id));
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

#[test]
fn logs_that_jobs_do_not_write_are_delivered_empty() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let workdir = folder.path().join("workflow");
    std::fs::create_dir(&workdir).expect("makes the folder");
    let snakefile = "rule all:\n    input: \"data.txt\"\n    log: \"logs/all.log\"\n\n\
                     rule A:\n    output: \"data.txt\"\n    log: \"logs/A.log\"\n    \
                     shell: \"touch {output}\"\n";
    std::fs::write(workdir.join("Snakefile"), snakefile).expect("writes the Snakefile");
    let launch = Launch {
        target: workdir.clone(),
        scratch: folder.path().join("scratch"),
        ..Launch::default()
    };
    let mut driver = snakemake_runtime::driver(&launch).expect("a driver");
    let made = folder.path().join("made");
    std::fs::create_dir(&made).expect("makes the folder");
    let mut executor = Quiet(made);
    let mut targets = Targets(
        Vec::new(),
        workdir.canonicalize().unwrap_or(workdir.clone()),
    );
    let execution = Engine::new(&mut executor)
        .observer(&mut targets)
        .run("Snakefile", driver.as_mut());
    assert!(execution.error.is_none(), "{:?}", execution.error);
    // Snakemake 9.27 leaves both logs, empty, as its test_github_issue2142 shows.
    targets.0.sort();
    assert_eq!(targets.0, ["data.txt", "logs/A.log", "logs/all.log"]);
}

/// Makes each directory output with a file `inner.txt` in it, and the other outputs, and notes
/// the steps it ran.
struct Folders(std::path::PathBuf, Vec<String>);

impl Executor for Folders {
    fn run(&mut self, attempt: Attempt) -> Running {
        let task = &attempt.task;
        self.1.push(task.step.clone());
        let mut files = Vec::new();
        for output in &task.outputs {
            let location = self.0.join(format!("{}", task.id));
            if output.ends_with('/') {
                std::fs::create_dir_all(&location).expect("makes the folder");
                std::fs::write(location.join("inner.txt"), "x\n").expect("writes");
            } else {
                std::fs::write(&location, "x\n").expect("writes");
            }
            files.push(OutputFile {
                path: output.clone(),
                location: location.to_string_lossy().into_owned(),
                directory: output.ends_with('/'),
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

#[test]
fn files_in_a_made_folder_come_from_its_job() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let workdir = folder.path().join("workflow");
    std::fs::create_dir(&workdir).expect("makes the folder");
    // `make` could also make `made/s/inner.txt` with x=s/inner.txt; Snakemake 9.27 plans no
    // such job, as the file is in the folder that the checkpoint made (test_checkpoints_many).
    let snakefile = "def inner(wildcards):\n    folder = checkpoints.make.get(x=\"s\").output[0]\n    \
                     return f\"{folder}/inner.txt\"\n\nrule all:\n    input: inner\n\n\
                     checkpoint make:\n    input: \"src/{x}.txt\"\n    \
                     output: directory(\"made/{x}\")\n    \
                     shell: \"mkdir -p {output} && touch {output}/inner.txt\"\n\n\
                     rule src:\n    output: \"src/{x}.txt\"\n    shell: \"touch {output}\"\n";
    std::fs::write(workdir.join("Snakefile"), snakefile).expect("writes the Snakefile");
    let launch = Launch {
        target: workdir.clone(),
        scratch: folder.path().join("scratch"),
        ..Launch::default()
    };
    let mut driver = snakemake_runtime::driver(&launch).expect("a driver");
    let made = folder.path().join("made");
    std::fs::create_dir(&made).expect("makes the folder");
    let mut executor = Folders(made, Vec::new());
    let execution = Engine::new(&mut executor).run("Snakefile", driver.as_mut());
    assert!(execution.error.is_none(), "{:?}", execution.error);
    assert_eq!(executor.1, ["src", "make", "all"]);
}
