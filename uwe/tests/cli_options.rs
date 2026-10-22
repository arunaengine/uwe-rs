//! Command-specific options, help and version output.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;

#[test]
fn rejects_misplaced_options() {
    for words in [
        ["runs", "-remove"],
        ["clean", "-executor"],
        ["page", "--sample"],
        ["version", "-quiet"],
        ["config", "-lang"],
        ["repeat", "-no-network"],
    ] {
        let output = common::command().args(words).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&output.stderr).contains("does not apply"));
    }
}

#[test]
fn discovery_skips_configuration() {
    for words in [vec!["run", "-help"], vec!["help", "run"]] {
        let output = common::command()
            .args(words)
            .env("UWE_SITE_CONFIG", "/missing.toml")
            .output()
            .unwrap();
        assert!(output.status.success());
        let help = String::from_utf8_lossy(&output.stdout);
        assert!(help.contains("usage: uwe run"));
        assert!(help.contains("-executor"));
        assert!(help.contains("Default executor: local"));
        assert!(help.contains("-profile <a,b>"));
        assert!(!help.contains("-remove"));
    }
    let output = common::command().arg("help").output().unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("local (the default for run)"));
    for token in [
        "uwe digest",
        "uwe reconcile",
        "-strict-provenance",
        "-front",
    ] {
        assert!(String::from_utf8_lossy(&output.stdout).contains(token));
    }
    for flag in ["version", "-version", "--version"] {
        let output = common::command().arg(flag).output().unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            format!("uwe {}\n", env!("CARGO_PKG_VERSION"))
        );
    }
    let output = common::command()
        .args(["version", "unexpected"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("takes no target"));
    let output = common::command()
        .args(["run", "-version"])
        .output()
        .unwrap();
    assert!(output.status.success());
}

#[test]
fn digests_writable_candidates() {
    let root = tempfile::tempdir().unwrap();
    let tree = root.path().join("reference");
    std::fs::create_dir(&tree).unwrap();
    let file = tree.join("data");
    std::fs::write(&file, "before").unwrap();
    let digest = |path: &std::path::Path| {
        let output = common::command()
            .arg("digest")
            .arg(path)
            .env("XDG_CACHE_HOME", root.path().join("cache"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    assert_eq!(
        digest(&file),
        task_model::Digest::sha256(b"before").to_string()
    );
    let before = digest(&tree);
    std::fs::write(&file, "after").unwrap();
    assert_ne!(digest(&tree), before);
    assert!(!std::fs::metadata(&tree).unwrap().permissions().readonly());
    assert!(!std::fs::metadata(&file).unwrap().permissions().readonly());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "after");
    assert!(!root.path().join("cache").exists());
}

fn owner_record(path: &std::path::Path) -> run_database::Recorder {
    run_database::Database::open(path)
        .unwrap()
        .record(&run_database::RunStart {
            id: "owned".into(),
            resumes: None,
            workflow: "/workflow/main.nf".into(),
            language: workflow_graph::Language::Nextflow,
            revision: None,
            arguments: Vec::new(),
            parameters: Vec::new(),
            executor: "local".into(),
            simulated: false,
            store: None,
            program: None,
            launch: None,
            workflow_folder: None,
            repeats: None,
        })
        .unwrap()
}

#[test]
fn leaves_running_owner() {
    if let Some(path) = std::env::var_os("UWE_RECONCILE_DATABASE") {
        std::mem::forget(owner_record(std::path::Path::new(&path)));
    }
}

#[test]
fn reconciles_stopped_owner() {
    let root = tempfile::tempdir().unwrap();
    let live = root.path().join("live.sqlite");
    let mut recorder = owner_record(&live);
    let command = |path: &std::path::Path, reason: Option<&str>| {
        let mut command = common::command();
        command.args(["reconcile", "owned", "-database"]).arg(path);
        if let Some(reason) = reason {
            command.args(["-reason", reason]);
        }
        command.output().unwrap()
    };
    assert_eq!(command(&live, None).status.code(), Some(2));
    assert_eq!(command(&live, Some("  ")).status.code(), Some(2));
    let rejected = command(&live, Some("owner exited"));
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("not confirmed stopped"));
    assert_eq!(
        run_database::Database::open(&live)
            .unwrap()
            .run("owned")
            .unwrap()
            .unwrap()
            .state,
        "running"
    );
    recorder.ended(None);
    recorder.close().unwrap();
    let dead = root.path().join("dead.sqlite");
    assert!(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "leaves_running_owner"])
            .env("UWE_RECONCILE_DATABASE", &dead)
            .status()
            .unwrap()
            .success()
    );
    let reconciled = command(&dead, Some("owner process exited"));
    assert!(
        reconciled.status.success(),
        "{}",
        String::from_utf8_lossy(&reconciled.stderr)
    );
    let database = run_database::Database::open(&dead).unwrap();
    assert!(database.owner("owned").unwrap().is_none());
    assert_eq!(
        database.run("owned").unwrap().unwrap().error.as_deref(),
        Some("owner process exited")
    );
    assert!(!command(&dead, Some("duplicate")).status.success());
}

#[test]
fn logging_respects_filters() {
    let root = tempfile::tempdir().unwrap();
    let output = common::command()
        .args(["runs", "-verbose"])
        .env("UWE_LOG", "invalid[")
        .env("XDG_CACHE_HOME", root.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let output = common::command()
        .args(["runs", "-quiet"])
        .env("UWE_LOG", "invalid[")
        .env("XDG_CACHE_HOME", root.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let output = common::command()
        .arg("runs")
        .env("UWE_LOG", "invalid[")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid UWE_LOG"));
    assert!(output.stdout.is_empty());
}

#[test]
fn target_controls_apply() {
    let folder = tempfile::tempdir().unwrap();
    std::fs::write(
        folder.path().join("Snakefile"),
        r#"
rule all:
    input: "last.txt", "side"
rule first:
    output: "first.txt"
    shell: "touch {output}"
rule last:
    input: "first.txt"
    output: "last.txt"
    shell: "cp {input} {output}"
rule side:
    output: "side"
    shell: "touch {output}"
"#,
    )
    .unwrap();
    let run = |options: &[&str]| {
        let output = common::command()
            .args(["graph", "Snakefile", "-format", "json"])
            .args(options)
            .current_dir(folder.path())
            .env("XDG_CACHE_HOME", folder.path().join("cache"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<workflow_graph::ExecutionGraph>(&output.stdout).unwrap()
    };
    let names = |graph: &workflow_graph::ExecutionGraph| {
        let mut names: Vec<_> = graph
            .nodes
            .iter()
            .map(|node| node.task.step.as_str())
            .collect();
        names.sort();
        names.into_iter().map(str::to_owned).collect::<Vec<_>>()
    };
    for selector in ["first", "first.txt", "./first.txt", "unused/../first.txt"] {
        assert_eq!(names(&run(&["-until", selector])), ["first"]);
    }
    assert_eq!(names(&run(&["-omit-from", "last"])), ["first", "side"]);
    assert_eq!(
        names(&run(&["-omit-from", "./last.txt"])),
        ["first", "side"]
    );
    let original = run(&[]);
    let resumed = run(&["-resume"]);
    assert_eq!(resumed.reused(), original.nodes.len());
    let forced = run(&["-resume", "-forcerun", "./first.txt"]);
    assert_eq!(names(&forced), ["all", "first", "last", "side"]);
    for node in &forced.nodes {
        assert_eq!(node.reused, node.task.step == "side");
        assert_eq!(
            node.task.reuse,
            if node.task.step == "side" {
                workflow_graph::Reuse::Allowed
            } else {
                workflow_graph::Reuse::Denied
            }
        );
    }
    let all = run(&["-resume", "-forceall"]);
    assert_eq!(all.reused(), 0);
    assert!(
        all.nodes
            .iter()
            .all(|node| node.task.reuse == workflow_graph::Reuse::Denied)
    );

    for language in ["nextflow", "cwl", "argo"] {
        for options in [
            vec!["-until", "first"],
            vec!["-omit-from", "first"],
            vec!["-forcerun", "first"],
            vec!["-forceall"],
        ] {
            let output = common::command()
                .args([
                    "run",
                    "-executor",
                    "simulator",
                    "Snakefile",
                    "-lang",
                    language,
                    "-resume",
                    "missing-run",
                ])
                .args(options)
                .current_dir(folder.path())
                .env("XDG_CACHE_HOME", folder.path().join("cache"))
                .output()
                .unwrap();
            assert_eq!(
                output.status.code(),
                Some(33),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("require a Snakemake workflow")
            );
        }
    }
}
