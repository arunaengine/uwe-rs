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
    /// Answers every task from a stored result that holds `cached`, as a cache hit does.
    cached: bool,
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
        if self.cached {
            let mut files = Vec::new();
            for (number, output) in task.outputs.iter().enumerate() {
                let location = self.store.join(format!("cached-{}-{number}", task.id));
                std::fs::write(&location, "cached").expect("a stored output");
                // The store keeps its files read-only.
                let mut permissions = std::fs::metadata(&location).unwrap().permissions();
                permissions.set_readonly(true);
                std::fs::set_permissions(&location, permissions).unwrap();
                files.push(OutputFile {
                    path: output.clone(),
                    location: location.to_string_lossy().into_owned(),
                    ..OutputFile::default()
                });
            }
            return Running::ended(Ok(Outcome {
                id: task.id,
                files,
                cached: true,
                ..Outcome::default()
            }));
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
        .find(|input| input.writable == Writable::Shared);
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

/// The declared inputs of a `copy-minimal` shadow count for the cache key like any other: the
/// task names each one where it lies, so that a changed input is not a stale stored result.
#[test]
fn keys_copied_inputs() {
    let snakefile = r#"
rule all:
    input: "b.txt"
rule copy:
    input: "a.txt"
    output: "b.txt"
    shadow: "copy-minimal"
    shell: "cat {input} > {output}"
"#;
    let steps: [(&str, Step); 1] = [("copy", |folder| {
        std::fs::copy(folder.join("a.txt"), folder.join("b.txt")).unwrap();
        0
    })];
    let folder = folder();
    std::fs::write(folder.path().join("work/a.txt"), "x").unwrap();
    let (folder, host, error) = run(folder, snakefile, &steps);
    assert_eq!(error, None);
    let work = folder.path().join("work");
    let task = host.tasks.iter().find(|task| task.step == "copy").unwrap();
    let input = task
        .inputs
        .iter()
        .find(|input| Path::new(&input.source) == work.join("a.txt"));
    assert_eq!(
        input.map(|input| input.writable),
        Some(Writable::No),
        "{:?}",
        task.inputs
    );
}

/// The current content of an `update()` output counts for the cache key of a shared job, so
/// that a stored result of a run that began with other content never replaces the file.
#[test]
fn keys_updated_outputs() {
    let snakefile = r#"
rule add:
    input: "in.txt"
    output: update("acc.txt")
    shell: "echo more >> {output}"
"#;
    let steps: [(&str, Step); 1] = [("add", |folder| {
        let old = std::fs::read_to_string(folder.join("acc.txt")).unwrap();
        std::fs::write(folder.join("acc.txt"), old + "more\n").unwrap();
        0
    })];
    let folder = folder();
    let acc = folder.path().join("work/acc.txt");
    std::fs::write(&acc, "first\n").unwrap();
    let hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    let file = std::fs::File::open(&acc).unwrap();
    file.set_modified(hour_ago).unwrap();
    std::fs::write(folder.path().join("work/in.txt"), "new").unwrap();
    let (folder, host, error) = run(folder, snakefile, &steps);
    assert_eq!(error, None);
    let work = folder.path().join("work");
    let task = host.tasks.iter().find(|task| task.step == "add").unwrap();
    let keyed = (task.inputs.iter()).any(|input| Path::new(&input.source) == work.join("acc.txt"));
    assert!(keyed, "{:?}", task.inputs);
    let text = std::fs::read_to_string(work.join("acc.txt")).unwrap();
    assert_eq!(text, "first\nmore\n");
    assert!(!work.join(".snakemake/backups/acc.txt").exists());
}

/// As Snakemake 9.27 does, an output without `directory()` that is a folder with files stays
/// before and after its job: the job then fails, and the files are still there.
#[test]
fn keeps_full_folders() {
    let snakefile = "rule a:\n    input: 'in.txt'\n    output: 'out'\n    shell: 'unused'\n";
    let folder = folder();
    let out = folder.path().join("work/out");
    std::fs::create_dir(&out).unwrap();
    std::fs::write(out.join("keep.txt"), "x").unwrap();
    let hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    let opened = std::fs::File::open(&out).unwrap();
    opened.set_modified(hour_ago).unwrap();
    std::fs::write(folder.path().join("work/in.txt"), "new").unwrap();
    let (folder, _host, error) = run(folder, snakefile, &[]);
    let error = error.expect("the run fails");
    assert!(error.contains("ImproperOutputException"), "{error}");
    let kept = folder.path().join("work/out/keep.txt");
    assert_eq!(std::fs::read_to_string(kept).unwrap(), "x");
}

/// As Snakemake 9.27 does, an output that is there and read-only stops the run before any
/// job, also without `protected()`, and the file stays as it was.
#[test]
fn refuses_readonly_outputs() {
    let snakefile = "rule a:\n    input: 'in.txt'\n    output: 'out.txt'\n    shell: 'unused'\n";
    let folder = folder();
    let out = folder.path().join("work/out.txt");
    std::fs::write(&out, "old").unwrap();
    let mut permissions = std::fs::metadata(&out).unwrap().permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&out, permissions).unwrap();
    // Root may write any file, so Snakemake protects nothing from it.
    if std::fs::OpenOptions::new().append(true).open(&out).is_ok() {
        return;
    }
    let hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    let opened = std::fs::File::open(&out).unwrap();
    opened.set_modified(hour_ago).unwrap();
    std::fs::write(folder.path().join("work/in.txt"), "new").unwrap();
    let (folder, host, error) = run(folder, snakefile, &[]);
    let error = error.expect("the run fails");
    assert!(error.contains("ProtectedOutputException"), "{error}");
    assert!(host.tasks.is_empty());
    let out = folder.path().join("work/out.txt");
    assert_eq!(std::fs::read_to_string(out).unwrap(), "old");
}

/// As Snakemake 9.27 does, a failed job gives its `update()` output the content from before
/// the job, from the backup in `.snakemake/backups`, which then goes.
#[test]
fn restores_failed_updates() {
    let snakefile =
        "rule add:\n    input: 'in.txt'\n    output: update('acc.txt')\n    shell: 'unused'\n";
    let steps: [(&str, Step); 1] = [("add", |folder| {
        std::fs::write(folder.join("acc.txt"), "half").unwrap();
        1
    })];
    let folder = folder();
    let acc = folder.path().join("work/acc.txt");
    std::fs::write(&acc, "first\n").unwrap();
    let hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    let opened = std::fs::File::open(&acc).unwrap();
    opened.set_modified(hour_ago).unwrap();
    std::fs::write(folder.path().join("work/in.txt"), "new").unwrap();
    let (folder, _host, error) = run(folder, snakefile, &steps);
    assert!(error.is_some());
    let work = folder.path().join("work");
    let text = std::fs::read_to_string(work.join("acc.txt")).unwrap();
    assert_eq!(text, "first\n");
    assert!(!work.join(".snakemake/backups/acc.txt").exists());
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

/// As in Snakemake's test_github_issue988, a job may write into the directory output of a
/// checkpoint that ended: Snakemake checks for outputs inside others only at the start.
#[test]
fn writes_checkpoint_folders() {
    let snakefile = r#"
rule all:
    input: "all.txt"
checkpoint make:
    output: directory("files")
    shell: "unused"
def reports(wildcards):
    folder = checkpoints.make.get().output[0]
    names, = glob_wildcards(folder + "/{name}.fasta")
    return expand(folder + "/{name}.faa", name=names)
rule pattern:
    input: "{any}.fasta"
    output: "{any}.faa"
    shell: "unused"
rule gather:
    input: reports
    output: "all.txt"
    shell: "unused"
"#;
    let steps: [(&str, Step); 3] = [
        ("make", |folder| {
            std::fs::create_dir(folder.join("files")).unwrap();
            std::fs::write(folder.join("files/f0.fasta"), ">a").unwrap();
            0
        }),
        ("pattern", |folder| {
            std::fs::copy(folder.join("files/f0.fasta"), folder.join("files/f0.faa")).unwrap();
            0
        }),
        ("gather", |folder| {
            std::fs::write(folder.join("all.txt"), "").unwrap();
            0
        }),
    ];
    let (folder, _host, error) = run(folder(), snakefile, &steps);
    assert_eq!(error, None);
    assert!(folder.path().join("work/files/f0.faa").is_file());
}

/// As in Snakemake's test_service_jobs, the file of a service goes once the jobs that use it
/// ended.
#[test]
fn removes_service_files() {
    let snakefile = r#"
rule all:
    input: "test.txt"
rule a:
    output: service("foo.socket")
    shell: "unused"
rule b:
    input: "foo.socket"
    output: "test.txt"
    shell: "unused"
"#;
    let steps: [(&str, Step); 1] = [("a", |folder| {
        std::fs::write(folder.join("foo.socket"), "").unwrap();
        std::fs::write(folder.join("test.txt"), "").unwrap();
        0
    })];
    let (folder, host, error) = run(folder(), snakefile, &steps);
    assert_eq!(
        error,
        None,
        "{:?}",
        host.tasks.iter().map(|task| &task.step).collect::<Vec<_>>()
    );
    assert!(folder.path().join("work/test.txt").is_file());
    assert!(
        !folder.path().join("work/foo.socket").exists(),
        "the service file is gone"
    );
}

/// As in Snakemake's test_update_flag, a job that reads a file as it was before an update
/// runs before the job that updates the file.
#[test]
fn reads_before_updates() {
    let snakefile = r#"
rule all:
    input: "out.txt", "test.txt"
rule b:
    input: before_update("test.txt")
    output: "out.txt"
    shell: "unused"
rule a:
    input: "in.txt"
    output: update("test.txt")
    shell: "unused"
"#;
    let steps: [(&str, Step); 2] = [
        ("b", |folder| {
            std::fs::copy(folder.join("test.txt"), folder.join("out.txt")).unwrap();
            0
        }),
        ("a", |folder| {
            let old = std::fs::read_to_string(folder.join("test.txt")).unwrap();
            std::fs::write(folder.join("test.txt"), old + "bar\n").unwrap();
            0
        }),
    ];
    let folder = folder();
    let test = folder.path().join("work/test.txt");
    std::fs::write(&test, "foo\n").unwrap();
    let hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    std::fs::File::open(&test)
        .unwrap()
        .set_modified(hour_ago)
        .unwrap();
    std::fs::write(folder.path().join("work/in.txt"), "new").unwrap();
    std::fs::write(folder.path().join("work/Snakefile"), snakefile).unwrap();
    let launch = Launch {
        target: folder.path().join("work"),
        scratch: folder.path().join("scratch"),
        ..Launch::default()
    };
    let options = snakemake_runtime::RunSettings {
        shared_workdir: true,
        ..Default::default()
    };
    let mut driver = snakemake_runtime::driver_with_options(&launch, Default::default(), options)
        .expect("a driver");
    let first: Vec<String> = driver
        .start()
        .unwrap()
        .into_iter()
        .map(|task| task.step)
        .collect();
    assert_eq!(first, ["b"], "the update waits for the reader");
    drop(driver);
    let (folder, host, error) = run(folder, snakefile, &steps);
    assert_eq!(error, None);
    let order: Vec<&str> = host.tasks.iter().map(|task| task.step.as_str()).collect();
    assert_eq!(&order[..2], ["b", "a"]);
    let out = std::fs::read_to_string(folder.path().join("work/out.txt")).unwrap();
    assert_eq!(out, "foo\n");
}

/// A job that a stored result answers gets its outputs back in the working directory, with
/// the time, protection and record of a job that ran, so that Snakemake's checks agree.
#[test]
fn restores_cached_outputs() {
    let folder = folder();
    let work = folder.path().join("work");
    let snakefile = "rule all:\n    input: 'kept.txt', 'plain.txt'\nrule keep:\n    output: protected('kept.txt')\n    shell: 'unused'\nrule plain:\n    output: 'plain.txt'\n    shell: 'unused'\n";
    std::fs::write(work.join("Snakefile"), snakefile).unwrap();
    let launch = Launch {
        target: work.clone(),
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
    std::fs::create_dir_all(&store).unwrap();
    let mut host = Host {
        steps: HashMap::new(),
        store,
        tasks: Vec::new(),
        cached: true,
    };
    let execution = Engine::new(&mut host).run("Snakefile", driver.as_mut());
    assert!(execution.error.is_none(), "{:?}", execution.error);
    assert_eq!(
        std::fs::read_to_string(work.join("kept.txt")).unwrap(),
        "cached"
    );
    assert!(
        std::fs::metadata(work.join("kept.txt"))
            .unwrap()
            .permissions()
            .readonly()
    );
    let plain = std::fs::metadata(work.join("plain.txt")).unwrap();
    assert!(
        !plain.permissions().readonly(),
        "a restored output may change"
    );
    let records = std::fs::read_dir(work.join(".snakemake/iunctus-signatures")).unwrap();
    assert_eq!(records.count(), 2, "the outputs are recorded");
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
        cached: false,
    };
    let execution = Engine::new(&mut host).run("Snakefile", driver.as_mut());
    let error = execution.error.map(|error| error.to_string());
    (folder, host, error)
}

/// As Snakemake 9.27 does, a run whose outputs another run of the folder locked stops before
/// any job, and leaves the lock of the other run; its own lock goes once it ended.
#[test]
fn respects_other_locks() {
    let snakefile = "rule a:\n    output: 'out.txt'\n    shell: 'unused'\n";
    let steps: [(&str, Step); 1] = [("a", |folder| {
        std::fs::write(folder.join("out.txt"), "made").unwrap();
        0
    })];
    let folder = folder();
    let locks = folder.path().join("work/.snakemake/locks");
    std::fs::create_dir_all(&locks).unwrap();
    std::fs::write(locks.join("0.output.lock"), "out.txt\n").unwrap();
    let (folder, host, error) = run(folder, snakefile, &steps);
    let error = error.expect("the run fails");
    assert!(error.contains("LockException"), "{error}");
    assert!(host.tasks.is_empty());
    std::fs::remove_file(locks.join("0.output.lock")).unwrap();
    std::fs::write(locks.join("0.output.lock"), "other.txt\n").unwrap();
    let (folder, _host, error) = run(folder, snakefile, &steps);
    assert_eq!(error, None);
    let names: Vec<String> = std::fs::read_dir(&locks)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        ["0.output.lock"],
        "only the lock of the other run stays"
    );
    assert!(folder.path().join("work/out.txt").is_file());
}

/// A profile with `nolock: true` neither checks nor takes locks, as Snakemake's `--nolock`.
#[test]
fn nolock_skips_locks() {
    let snakefile = "rule a:\n    output: 'out.txt'\n    shell: 'unused'\n";
    let steps: [(&str, Step); 1] = [("a", |folder| {
        std::fs::write(folder.join("out.txt"), "made").unwrap();
        0
    })];
    let folder = folder();
    let profile = folder.path().join("work/profiles/default");
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join("config.yaml"), "nolock: true\n").unwrap();
    let locks = folder.path().join("work/.snakemake/locks");
    std::fs::create_dir_all(&locks).unwrap();
    std::fs::write(locks.join("0.output.lock"), "out.txt\n").unwrap();
    let (folder, _host, error) = run(folder, snakefile, &steps);
    assert_eq!(error, None);
    assert!(folder.path().join("work/out.txt").is_file());
}

/// An output that Snakemake's own incomplete record names is made again, as with
/// `--rerun-incomplete`.
#[test]
fn reruns_snakemake_incomplete() {
    let snakefile = "rule a:\n    output: 'out.txt'\n    shell: 'unused'\n";
    let steps: [(&str, Step); 1] = [("a", |folder| {
        std::fs::write(folder.join("out.txt"), "made").unwrap();
        0
    })];
    let folder = folder();
    std::fs::write(folder.path().join("work/out.txt"), "half").unwrap();
    // Snakemake names the record by the URL-safe Base64 of the path.
    let marks = folder.path().join("work/.snakemake/incomplete");
    std::fs::create_dir_all(&marks).unwrap();
    std::fs::write(marks.join("b3V0LnR4dA=="), "{}").unwrap();
    let (folder, _host, error) = run(folder, snakefile, &steps);
    assert_eq!(error, None);
    let text = std::fs::read_to_string(folder.path().join("work/out.txt")).unwrap();
    assert_eq!(text, "made");
}

/// A script that this run wrote below `.snakemake` stays while another run of the folder
/// holds the same file, and goes with the last run that holds it.
#[test]
fn keeps_held_scripts() {
    let snakefile = "rule a:\n    output: 'out.txt'\n    script: 'make.py'\n";
    let steps: [(&str, Step); 1] = [("a", |folder| {
        // Another run holds each written script as `hold` does: by a link of its own.
        for entry in std::fs::read_dir(folder.join(".snakemake/scripts")).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if name.starts_with("iunctus-") && !name.contains(".iunctus-") {
                let mut other = path.clone().into_os_string();
                other.push(".iunctus-other");
                std::fs::hard_link(&path, other).unwrap();
            }
        }
        std::fs::write(folder.join("out.txt"), "made").unwrap();
        0
    })];
    let folder = folder();
    std::fs::write(folder.path().join("work/make.py"), "print(1)\n").unwrap();
    let (folder, _host, error) = run(folder, snakefile, &steps);
    assert_eq!(error, None);
    let scripts = folder.path().join("work/.snakemake/scripts");
    let names: Vec<String> = std::fs::read_dir(&scripts)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    let script = names.iter().find(|name| name.ends_with("make.py"));
    assert!(
        script.is_some(),
        "the script of the other run stays: {names:?}"
    );
    assert!(
        names
            .iter()
            .all(|name| !name.contains(".iunctus-") || name.ends_with("-other"))
    );
}

/// An output of a shadow job whose folder is a link to another file system moves there as
/// `shutil.move` does, by a copy, and the shadow directory goes.
#[test]
fn moves_across_devices() {
    use std::os::unix::fs::MetadataExt;
    let snakefile =
        "rule a:\n    output: 'out/x.txt'\n    shadow: 'minimal'\n    shell: 'unused'\n";
    let steps: [(&str, Step); 1] = [("a", |folder| {
        std::fs::write(folder.join("out/x.txt"), "made").unwrap();
        0
    })];
    let folder = folder();
    let other = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).expect("another folder");
    let devices = [folder.path(), other.path()].map(|path| std::fs::metadata(path).unwrap().dev());
    if devices[0] == devices[1] {
        return;
    }
    std::os::unix::fs::symlink(other.path(), folder.path().join("work/out")).unwrap();
    let (folder, _host, error) = run(folder, snakefile, &steps);
    assert_eq!(error, None);
    assert_eq!(
        std::fs::read_to_string(other.path().join("x.txt")).unwrap(),
        "made"
    );
    let shadows = std::fs::read_dir(folder.path().join("work/.snakemake/shadow")).unwrap();
    assert_eq!(shadows.count(), 0, "the shadow directory is gone");
}

/// Answers each task from a stored folder with a file, a folder and links to both, as a
/// cache hit of a `directory()` output does.
struct Folders(PathBuf);

impl Executor for Folders {
    fn run(&mut self, attempt: Attempt) -> Running {
        let task = &attempt.task;
        let mut files = Vec::new();
        for (number, output) in task.outputs.iter().enumerate() {
            let location = self.0.join(format!("{}-{number}", task.id));
            std::fs::create_dir_all(location.join("sub")).unwrap();
            std::fs::write(location.join("a.txt"), "a").unwrap();
            std::os::unix::fs::symlink("a.txt", location.join("b.txt")).unwrap();
            std::os::unix::fs::symlink("sub", location.join("s")).unwrap();
            std::os::unix::fs::symlink("/etc/hostname", location.join("out")).unwrap();
            files.push(OutputFile {
                path: output.clone(),
                location: location.to_string_lossy().into_owned(),
                directory: true,
                ..OutputFile::default()
            });
        }
        Running::ended(Ok(Outcome {
            id: task.id,
            files,
            cached: true,
            ..Outcome::default()
        }))
    }
}

/// A stored `directory()` output comes back with its links inside the working directory as
/// links, also those to folders; a link that leads out becomes a copy of what it names.
#[test]
fn restores_folder_links() {
    let folder = folder();
    let work = folder.path().join("work");
    let snakefile = "rule a:\n    output: directory('d')\n    shell: 'unused'\n";
    std::fs::write(work.join("Snakefile"), snakefile).unwrap();
    let launch = Launch {
        target: work.clone(),
        scratch: folder.path().join("scratch"),
        ..Launch::default()
    };
    let options = snakemake_runtime::RunSettings {
        shared_workdir: true,
        ..Default::default()
    };
    let mut driver = snakemake_runtime::driver_with_options(&launch, Default::default(), options)
        .expect("a driver");
    let mut executor = Folders(folder.path().join("store"));
    let execution = Engine::new(&mut executor).run("Snakefile", driver.as_mut());
    assert!(execution.error.is_none(), "{:?}", execution.error);
    let link = |name: &str| std::fs::read_link(work.join("d").join(name)).ok();
    assert_eq!(link("b.txt"), Some(PathBuf::from("a.txt")));
    assert_eq!(link("s"), Some(PathBuf::from("sub")));
    assert_eq!(link("out"), None, "a link that leads out is a copy");
    assert!(work.join("d/sub").is_dir());
}

/// As in Snakemake 9.27, outputs may lie outside the working directory, by `..` or by an
/// absolute path: the job gets the folder of each such output, and only that folder.
#[test]
fn writes_outside_outputs() {
    let folder = folder();
    let absolute = folder.path().join("abs/y.txt");
    let snakefile = format!(
        "rule all:\n    input: '../outside/x.txt', '{0}'\nrule a:\n    output: '../outside/x.txt'\n    shell: 'unused'\nrule b:\n    output: '{0}'\n    shell: 'unused'\n",
        absolute.display()
    );
    let steps: [(&str, Step); 2] = [
        ("a", |folder| {
            std::fs::write(folder.join("../outside/x.txt"), "x").unwrap();
            0
        }),
        ("b", |folder| {
            std::fs::write(folder.parent().unwrap().join("abs/y.txt"), "y").unwrap();
            0
        }),
    ];
    let (folder, host, error) = run(folder, &snakefile, &steps);
    assert_eq!(error, None);
    for (step, mounted, output) in [("a", "outside", "outside/x.txt"), ("b", "abs", "abs/y.txt")] {
        let task = host.tasks.iter().find(|task| task.step == step).unwrap();
        let shared: Vec<&str> = (task.inputs.iter())
            .filter(|input| input.writable == Writable::Shared)
            .map(|input| input.path.as_str())
            .collect();
        let mounted = folder.path().join(mounted);
        assert!(shared.contains(&mounted.to_str().unwrap()), "{shared:?}");
        assert_eq!(shared.len(), 2, "the working directory and that folder");
        assert_eq!(task.outputs, [folder.path().join(output).to_string_lossy()]);
    }
}
