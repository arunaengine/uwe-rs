//! Checks jobs that run in the one working directory that they share, as in Snakemake.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use workflow_graph::{Attempt, Engine, Executor, Launch, Outcome, OutputFile, Running, Writable};

/// What a job does in the folder where it starts; its exit code.
type Step = fn(&Path) -> i32;

/// Runs each task as its rule's step in the folder where the task starts, and collects the
/// outputs that are there, as the local executor does.
struct Host {
    steps: HashMap<&'static str, Step>,
    store: PathBuf,
    tasks: Vec<workflow_graph::Task>,
}

impl Executor for Host {
    fn run(&mut self, attempt: Attempt) -> Running {
        let task = (*attempt.task).clone();
        self.tasks.push(task.clone());
        let mut folder = PathBuf::from(task.workdir.clone().unwrap_or_default());
        // A job of a shadow directory changes into it first.
        if task.command.first().is_some_and(|word| word == "sh")
            && task
                .command
                .get(2)
                .is_some_and(|word| word.contains("cd \"$1\""))
        {
            folder = PathBuf::from(&task.command[4]);
        }
        let exit_code = match self.steps.get(task.step.as_str()) {
            Some(step) => step(&folder),
            None => 0,
        };
        let mut files = Vec::new();
        for output in &task.outputs {
            let path = Path::new(output.trim_end_matches('/'));
            let Ok(metadata) = std::fs::metadata(path) else {
                continue;
            };
            let location = self.store.join(format!("{}-{}", task.id, files.len()));
            if metadata.is_file() {
                std::fs::copy(path, &location).expect("copies an output");
            }
            files.push(OutputFile {
                path: output.clone(),
                location: location.to_string_lossy().into_owned(),
                directory: metadata.is_dir(),
                ..OutputFile::default()
            });
        }
        Running::ended(Ok(Outcome {
            id: task.id,
            exit_code,
            files,
            ..Outcome::default()
        }))
    }
}

/// A folder with an empty working directory `work`.
fn folder() -> tempfile::TempDir {
    let folder = tempfile::tempdir().expect("a folder");
    std::fs::create_dir_all(folder.path().join("work")).expect("a working directory");
    folder
}

#[test]
fn shares_undeclared_files() {
    let snakefile = r#"
rule all:
    input: "b.txt"
rule a:
    output: "a.txt", "link.txt"
    shell: "unused"
rule b:
    input: "a.txt"
    output: "b.txt"
    shell: "unused"
"#;
    let steps: [(&str, Step); 2] = [
        ("a", |folder| {
            std::fs::write(folder.join("a.txt"), "a\n").unwrap();
            std::fs::write(folder.join("undeclared.txt"), "hidden\n").unwrap();
            std::os::unix::fs::symlink("a.txt", folder.join("link.txt")).unwrap();
            0
        }),
        ("b", |folder| {
            let a = std::fs::read_to_string(folder.join("a.txt")).unwrap();
            let hidden = std::fs::read_to_string(folder.join("undeclared.txt")).unwrap();
            std::fs::write(folder.join("b.txt"), a + &hidden).unwrap();
            0
        }),
    ];
    let (folder, host, error) = run(folder(), snakefile, &steps);
    assert_eq!(error, None);
    let work = folder.path().join("work");
    assert_eq!(
        std::fs::read_to_string(work.join("b.txt")).unwrap(),
        "a\nhidden\n"
    );
    assert!(
        std::fs::symlink_metadata(work.join("link.txt"))
            .unwrap()
            .is_symlink()
    );
    let task = host.tasks.iter().find(|task| task.step == "b").unwrap();
    let place = task
        .inputs
        .iter()
        .find(|input| input.writable == Writable::InPlace);
    assert_eq!(
        place.map(|input| Path::new(&input.source)),
        Some(work.as_path())
    );
    assert_eq!(task.workdir.as_deref().map(Path::new), Some(work.as_path()));
    // The job sees the time zone of this machine, as Snakemake's commands do.
    let zone = task
        .inputs
        .iter()
        .any(|input| input.path == "/etc/localtime");
    assert_eq!(zone, Path::new("/etc/localtime").exists());
    assert_eq!(task.outputs, [work.join("b.txt").to_string_lossy()]);
}

/// Snakemake 9.27 fails test_symlink_temp: the temporary file is gone once the job after it
/// ended, so the link to it that the next job reads points nowhere.
#[test]
fn removes_temporary_targets() {
    let snakefile = r#"
rule all:
    input: "3.a"
rule one:
    output: temp("1.a")
    shell: "unused"
rule two:
    input: "1.a"
    output: temp("2.a")
    shell: "unused"
rule three:
    input: "2.a"
    output: "3.a"
    shell: "unused"
"#;
    let steps: [(&str, Step); 3] = [
        ("one", |folder| {
            std::fs::write(folder.join("1.a"), "").unwrap();
            0
        }),
        ("two", |folder| {
            std::os::unix::fs::symlink("1.a", folder.join("2.a")).unwrap();
            0
        }),
        ("three", |folder| {
            std::fs::write(folder.join("3.a"), "").unwrap();
            0
        }),
    ];
    let (folder, host, error) = run(folder(), snakefile, &steps);
    assert!(
        !folder.path().join("work/1.a").exists(),
        "the temporary file is gone"
    );
    let error = error.expect("the run fails");
    assert!(
        error.contains("Missing files") && error.contains("2.a"),
        "{error}"
    );
    assert!(host.tasks.iter().all(|task| task.step != "three"));
}

#[test]
fn moves_shadow_outputs() {
    let snakefile = r#"
rule all:
    input: "out/result.txt"
rule shadowed:
    input: "in.txt"
    output: "out/result.txt"
    log: "logs/shadowed.log"
    shadow: "copy-minimal"
    shell: "unused"
"#;
    let steps: [(&str, Step); 1] = [("shadowed", |folder| {
        let input = folder.join("in.txt");
        let copied = !std::fs::symlink_metadata(&input).unwrap().is_symlink();
        std::fs::write(folder.join("junk.txt"), "").unwrap();
        std::fs::write(folder.join("out/result.txt"), format!("{copied}")).unwrap();
        std::fs::write(folder.join("logs/shadowed.log"), "log").unwrap();
        0
    })];
    let folder = folder();
    std::fs::write(folder.path().join("work/in.txt"), "x").unwrap();
    let (other, _host, error) = run(folder, snakefile, &steps);
    assert_eq!(error, None);
    let work = other.path().join("work");
    assert_eq!(
        std::fs::read_to_string(work.join("out/result.txt")).unwrap(),
        "true"
    );
    assert!(work.join("logs/shadowed.log").is_file());
    assert!(!work.join("junk.txt").exists());
    let shadows = std::fs::read_dir(work.join(".snakemake/shadow"))
        .unwrap()
        .count();
    assert_eq!(shadows, 0, "the shadow directory is gone");
}

#[test]
fn removes_failed_outputs() {
    let snakefile =
        "rule broken:\n    output: 'half.txt'\n    log: 'broken.log'\n    shell: 'unused'\n";
    let steps: [(&str, Step); 1] = [("broken", |folder| {
        std::fs::write(folder.join("half.txt"), "half").unwrap();
        std::fs::write(folder.join("broken.log"), "why").unwrap();
        1
    })];
    let (folder, _host, error) = run(folder(), snakefile, &steps);
    assert!(error.is_some());
    let work = folder.path().join("work");
    assert!(
        !work.join("half.txt").exists(),
        "Snakemake removes the outputs"
    );
    assert!(work.join("broken.log").exists(), "and keeps the log");
}

#[test]
fn touches_shared_outputs() {
    let snakefile = r#"
rule all:
    input: "flag", "kept.txt"
rule mark:
    output: touch("flag")
    shell: "unused"
rule keep:
    output: protected("kept.txt")
    shell: "unused"
"#;
    let steps: [(&str, Step); 1] = [("keep", |folder| {
        std::fs::write(folder.join("kept.txt"), "kept").unwrap();
        0
    })];
    let (folder, _host, error) = run(folder(), snakefile, &steps);
    assert_eq!(error, None);
    let work = folder.path().join("work");
    assert!(work.join("flag").is_file());
    let kept = std::fs::metadata(work.join("kept.txt")).unwrap();
    assert!(kept.permissions().readonly(), "protected");
    let records = std::fs::read_dir(work.join(".snakemake/iunctus-signatures")).unwrap();
    assert_eq!(records.count(), 2, "both outputs are recorded");
}

/// As in Snakemake's test_directory, a directory output gets `.snakemake_timestamp`, and an
/// output that is a directory without `directory()` fails its job.
#[test]
fn checks_output_kinds() {
    let snakefile = r#"
rule all:
    input: "made", "plain"
rule made:
    output: directory("made")
    shell: "unused"
rule plain:
    output: "plain"
    shell: "unused"
"#;
    let steps: [(&str, Step); 2] = [
        ("made", |folder| {
            std::fs::create_dir(folder.join("made")).unwrap();
            0
        }),
        ("plain", |folder| {
            std::fs::create_dir(folder.join("plain")).unwrap();
            0
        }),
    ];
    let (folder, _host, error) = run(folder(), snakefile, &steps);
    let error = error.expect("the run fails");
    assert!(error.contains("ImproperOutputException"), "{error}");
    assert!(
        folder
            .path()
            .join("work/made/.snakemake_timestamp")
            .is_file()
    );
}

/// As in Snakemake's test_github_issue2732, a temporary input of a checkpoint goes once the
/// run planned again after the checkpoint.
#[test]
fn releases_checkpoint_inputs() {
    let snakefile = r#"
rule b1:
    output: temp("A.b1.out")
    shell: "unused"
checkpoint b2:
    input: "A.b1.out"
    output: "A.b2.out"
    shell: "unused"
def names(wildcards):
    return open(checkpoints.b2.get().output[0]).read().strip()
rule a1:
    output: "{name}.a1.out"
    shell: "unused"
rule all:
    input: lambda w: expand(rules.a1.output, name=names(w))
    default_target: True
"#;
    let steps: [(&str, Step); 3] = [
        ("b1", |folder| {
            std::fs::write(folder.join("A.b1.out"), "").unwrap();
            0
        }),
        ("b2", |folder| {
            std::fs::write(folder.join("A.b2.out"), "A\n").unwrap();
            0
        }),
        ("a1", |folder| {
            std::fs::write(folder.join("A.a1.out"), "").unwrap();
            0
        }),
    ];
    let (folder, _host, error) = run(folder(), snakefile, &steps);
    assert_eq!(error, None);
    let work = folder.path().join("work");
    assert!(work.join("A.a1.out").is_file());
    assert!(
        !work.join("A.b1.out").exists(),
        "the temporary input is gone"
    );
}

/// As in Snakemake's test_modules_all, the empty folder of a removed temporary file goes once
/// the run succeeded.
#[test]
fn removes_empty_folders() {
    let snakefile = r#"
rule all:
    input: "results/b.txt"
rule a:
    output: temp("results/a/a.txt")
    shell: "unused"
rule b:
    input: "results/a/a.txt"
    output: "results/b.txt"
    shell: "unused"
"#;
    let steps: [(&str, Step); 2] = [
        ("a", |folder| {
            std::fs::write(folder.join("results/a/a.txt"), "a").unwrap();
            0
        }),
        ("b", |folder| {
            std::fs::write(folder.join("results/b.txt"), "b").unwrap();
            0
        }),
    ];
    let (folder, _host, error) = run(folder(), snakefile, &steps);
    assert_eq!(error, None);
    let work = folder.path().join("work");
    assert!(work.join("results/b.txt").is_file());
    assert!(!work.join("results/a").exists(), "the empty folder is gone");
}

/// A job that failed leaves its outputs marked incomplete, so that the next run makes them
/// again even when a later job left one there; one that ended well leaves no mark.
#[test]
fn marks_running_outputs() {
    let snakefile = "rule all:\n    input: 'ok.txt', 'half.txt'\nrule good:\n    output: 'ok.txt'\n    shell: 'unused'\nrule broken:\n    output: 'half.txt'\n    shell: 'unused'\n";
    let steps: [(&str, Step); 2] = [
        ("good", |folder| {
            std::fs::write(folder.join("ok.txt"), "ok").unwrap();
            0
        }),
        ("broken", |folder| {
            std::fs::write(folder.join("half.txt"), "half").unwrap();
            1
        }),
    ];
    let (folder, _host, error) = run(folder(), snakefile, &steps);
    assert!(error.is_some());
    let marks = folder.path().join("work/.snakemake/iunctus-incomplete");
    let names: Vec<String> = std::fs::read_dir(&marks)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names.len(), 1, "only the failed output: {names:?}");
}

/// Runs a Snakefile in the working directory of `folder` with jobs in the shared directory;
/// the folder, the tasks and the error.
fn run(
    folder: tempfile::TempDir,
    snakefile: &str,
    steps: &[(&'static str, Step)],
) -> (tempfile::TempDir, Host, Option<String>) {
    let workdir = folder.path().join("work");
    std::fs::write(workdir.join("Snakefile"), snakefile).expect("a Snakefile");
    let launch = Launch {
        target: workdir,
        scratch: folder.path().join("scratch"),
        ..Launch::default()
    };
    let options = snakemake_runtime::RunSettings {
        shared_workdir: true,
        ..Default::default()
    };
    let mut driver = snakemake_runtime::driver_with_options(&launch, Default::default(), options)
        .expect("a driver");
    let store = folder.path().join("store");
    std::fs::create_dir_all(&store).expect("a store");
    let mut host = Host {
        steps: steps.iter().copied().collect(),
        store,
        tasks: Vec::new(),
    };
    let execution = Engine::new(&mut host).run("Snakefile", driver.as_mut());
    let error = execution.error.map(|error| error.to_string());
    (folder, host, error)
}
