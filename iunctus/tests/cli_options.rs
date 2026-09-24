//! Command-specific options, help and version output.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;

#[test]
fn commands_reject_options_from_other_commands() {
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
fn help_and_version_do_not_load_configuration() {
    for words in [vec!["run", "-help"], vec!["help", "run"]] {
        let output = common::command()
            .args(words)
            .env("IUNCTUS_SITE_CONFIG", "/missing.toml")
            .output()
            .unwrap();
        assert!(output.status.success());
        let help = String::from_utf8_lossy(&output.stdout);
        assert!(help.contains("usage: iunctus run"));
        assert!(help.contains("-executor"));
        assert!(help.contains("-profile <a,b>"));
        assert!(!help.contains("-remove"));
    }
    for flag in ["version", "-version", "--version"] {
        let output = common::command().arg(flag).output().unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            format!("iunctus {}\n", env!("CARGO_PKG_VERSION"))
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
fn logging_respects_filters() {
    let root = tempfile::tempdir().unwrap();
    let output = common::command()
        .args(["runs", "-verbose"])
        .env("IUNCTUS_LOG", "invalid[")
        .env("XDG_CACHE_HOME", root.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let output = common::command()
        .args(["runs", "-quiet"])
        .env("IUNCTUS_LOG", "invalid[")
        .env("XDG_CACHE_HOME", root.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let output = common::command()
        .arg("runs")
        .env("IUNCTUS_LOG", "invalid[")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid IUNCTUS_LOG"));
    assert!(output.stdout.is_empty());
}

#[test]
fn snakemake_target_controls_select_jobs_and_forcing_bypasses_resume() {
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
    for selector in ["first", "first.txt"] {
        assert_eq!(names(&run(&["-until", selector])), ["first"]);
    }
    assert_eq!(names(&run(&["-omit-from", "last"])), ["first", "side"]);
    let original = run(&[]);
    let resumed = run(&["-resume"]);
    assert_eq!(resumed.reused(), original.nodes.len());
    let forced = run(&["-resume", "-forcerun", "first"]);
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
                .args(["run", "Snakefile", "-lang", language])
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
