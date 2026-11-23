//! The grouped commands of `uwe runs`, `uwe tool` and `uwe cache`, POSIX options, JSON output,
//! `uwe doctor`, `uwe init` and the shell completions.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;
use std::process::Command;

use serde_json::Value;

use super::common;

/// Runs the binary in `folder`, with its cache there; its exit code, output and errors.
fn uwe(folder: &Path, words: &[&str]) -> (i32, String, String) {
    let output = common::command()
        .args(words)
        .current_dir(folder)
        .env("XDG_CACHE_HOME", folder.join("cache"))
        .output()
        .expect("runs uwe");
    let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
    (
        output.status.code().unwrap_or(-1),
        text(&output.stdout),
        text(&output.stderr),
    )
}

fn json(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|error| panic!("{error}: {text}"))
}

/// A folder with the starter workflow of `uwe init`, run once on the simulator; the run id.
fn started(folder: &Path) -> String {
    let (code, printed, errors) = uwe(folder, &["init", "--name", "demo"]);
    assert_eq!(code, 0, "{printed}{errors}");
    assert!(printed.contains("demo.uwe.yaml"), "{printed}");
    let words = [
        "run",
        "--executor",
        "simulator",
        "demo.uwe.yaml",
        "--inputs=inputs.json",
        "--",
    ];
    let (code, printed, errors) = uwe(folder, &words);
    assert_eq!(code, 0, "{printed}{errors}");
    let (code, listed, errors) = uwe(folder, &["runs", "--json"]);
    assert_eq!(code, 0, "{errors}");
    json(&listed)[0]["id"]
        .as_str()
        .expect("a run id")
        .to_owned()
}

#[test]
fn groups_run_commands() {
    let root = tempfile::tempdir().unwrap();
    let folder = root.path();
    let run = started(folder);
    let (code, listed, _) = uwe(folder, &["runs", "list"]);
    assert_eq!(code, 0);
    assert!(
        listed.starts_with("RUN") && listed.contains(&run),
        "{listed}"
    );
    let (code, shown, errors) = uwe(folder, &["runs", "show", &run]);
    assert_eq!(code, 0, "{errors}");
    assert!(
        shown.contains("state      succeeded") && shown.contains("count_lines"),
        "{shown}"
    );
    let (code, shown, _) = uwe(folder, &["runs", "show", &run, "-json"]);
    assert_eq!(code, 0);
    let shown = json(&shown);
    assert_eq!(
        (
            shown["run"]["id"].as_str(),
            shown["tasks"][0]["state"].as_str()
        ),
        (Some(run.as_str()), Some("succeeded"))
    );
    assert!(shown["inputs"]["texts"].is_array(), "{shown}");
    let (code, _, errors) = uwe(folder, &["runs", "logs", &run]);
    assert_eq!(code, 0, "{errors}");
    let (code, printed, errors) = uwe(folder, &["runs", "time", &run]);
    assert_eq!(code, 0, "{printed}{errors}");
    let (code, printed, _) = uwe(folder, &["runs", "cancel", &run]);
    assert_eq!(code, 0);
    assert!(
        printed.contains("has succeeded and left no remote jobs"),
        "{printed}"
    );
    let (code, _, errors) = uwe(folder, &["runs", "show", "nothing-here"]);
    assert_ne!(code, 0);
    assert!(errors.contains("there is no run nothing-here"), "{errors}");
    let (code, _, errors) = uwe(folder, &["runs", "bogus"]);
    assert_eq!(code, 2);
    assert!(
        errors.contains("unknown command `uwe runs bogus`"),
        "{errors}"
    );
    let (code, _, errors) = uwe(folder, &["cache"]);
    assert_eq!(code, 2);
    assert!(errors.contains("uwe cache <clean|digest>"), "{errors}");
    let (code, digest, _) = uwe(folder, &["cache", "digest", "example.txt", "--json"]);
    assert_eq!(code, 0);
    assert!(
        json(&digest)["digest"]
            .as_str()
            .unwrap()
            .starts_with("sha256:"),
        "{digest}"
    );
    let (code, help, _) = uwe(folder, &["help", "runs"]);
    assert_eq!(code, 0);
    assert!(
        help.contains("`uwe runs` alone is `uwe runs list`") && help.contains("cancel"),
        "{help}"
    );
    let (code, help, _) = uwe(folder, &["help", "runs", "show"]);
    assert_eq!(code, 0);
    assert!(
        help.contains("usage: uwe runs show [options] <run>") && help.contains("-json"),
        "{help}"
    );
}

#[test]
fn writes_starter_once() {
    let root = tempfile::tempdir().unwrap();
    let folder = root.path().join("new project");
    let (code, printed, errors) = uwe(root.path(), &["init", "new project"]);
    assert_eq!(code, 0, "{printed}{errors}");
    assert!(folder.join("new_project.uwe.yaml").is_file(), "{printed}");
    assert!(folder.join("tools/count_lines.cwl").is_file());
    let starter = folder.join("example.txt");
    std::fs::write(&starter, "kept\n").unwrap();
    let (code, _, errors) = uwe(root.path(), &["init", "new project"]);
    assert_eq!(code, 1);
    assert!(
        errors.contains("init writes nothing, as these files exist"),
        "{errors}"
    );
    assert_eq!(std::fs::read_to_string(&starter).unwrap(), "kept\n");
    let (code, _, errors) = uwe(root.path(), &["init", "-name", "no/slash"]);
    assert_eq!(code, 2, "{errors}");
}

#[test]
fn reads_tool_library() {
    let root = tempfile::tempdir().unwrap();
    let folder = root.path();
    started(folder);
    let (code, listed, errors) = uwe(folder, &["tool", "list", "--tools", "tools", "--json"]);
    assert_eq!(code, 0, "{errors}");
    let listed = json(&listed);
    assert_eq!(listed["tools"][0]["id"], "count_lines");
    assert_eq!(listed["tools"][0]["ecosystem"], "cwl");
    let (code, table, _) = uwe(folder, &["tool", "--tools=tools"]);
    assert_eq!(code, 0);
    assert!(
        table.starts_with("ID") && table.contains("Count lines"),
        "{table}"
    );
    let (code, shown, _) = uwe(
        folder,
        &["tool", "inspect", "count_lines", "--tools", "tools"],
    );
    assert_eq!(code, 0);
    assert!(
        shown.contains("inputs:\n  text") && shown.contains("image      debian:stable-slim"),
        "{shown}"
    );
    let (code, extracted, _) = uwe(
        folder,
        &["tool", "extract", "tools/count_lines.cwl", "--json"],
    );
    assert_eq!(code, 0);
    assert_eq!(json(&extracted)[0]["outputs"][0]["name"], "count");
    let (code, checked, errors) = uwe(folder, &["tool", "check", "demo.uwe.yaml"]);
    assert_eq!(code, 0, "{checked}{errors}");
    assert!(
        checked.contains("step count: fits, maps over a list"),
        "{checked}"
    );
    let (code, checked, _) = uwe(folder, &["tool", "check", "demo.uwe.yaml", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(json(&checked)["nodes"][0]["scatter"], true);
    std::fs::write(
        folder.join("tools/more.cwl"),
        std::fs::read_to_string(folder.join("tools/count_lines.cwl"))
            .unwrap()
            .replace("Count lines", "Count more"),
    )
    .unwrap();
    let (code, suggested, errors) = uwe(
        folder,
        &[
            "tool",
            "suggest",
            "count_lines",
            "count",
            "--tools",
            "tools",
            "--json",
        ],
    );
    assert_eq!(code, 0, "{errors}");
    let suggested = json(&suggested);
    assert_eq!(suggested["mode"], "followers");
    assert!(
        suggested["suggestions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["tool"] == "more"),
        "{suggested}"
    );
    let (code, found, _) = uwe(folder, &["tool", "suggest", "more", "--tools", "tools"]);
    assert_eq!(code, 0);
    assert!(
        found.contains("Count more") && !found.contains("Count lines"),
        "{found}"
    );
}

#[test]
fn reports_doctor_json() {
    let root = tempfile::tempdir().unwrap();
    let (code, printed, errors) = uwe(root.path(), &["doctor", "--json"]);
    assert!(matches!(code, 0 | 3), "{printed}{errors}");
    let report = json(&printed);
    assert_eq!(report["ready"], code == 0);
    let parts: Vec<&str> = (report["found"].as_array().unwrap().iter())
        .map(|found| found["part"].as_str().unwrap())
        .collect();
    assert!(parts.contains(&"container engine"), "{parts:?}");
    let (code, help, _) = uwe(root.path(), &["help", "doctor"]);
    assert_eq!(code, 0);
    assert!(
        help.contains("`uwe doctor` alone is `uwe check-machine`"),
        "{help}"
    );
}

/// Checks a completion script with the shell itself, when the machine has that shell.
fn shell_accepts(shell: &str, script: &str) {
    let found = Command::new("sh")
        .args(["-c", &format!("command -v {shell}")])
        .output()
        .unwrap();
    if !found.status.success() {
        eprintln!("{shell} is not installed; its script is not checked");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("completion");
    std::fs::write(&file, script).unwrap();
    let checked = Command::new(shell).arg("-n").arg(&file).output().unwrap();
    assert!(
        checked.status.success(),
        "{shell}: {}",
        String::from_utf8_lossy(&checked.stderr)
    );
}

#[test]
fn prints_completions() {
    let root = tempfile::tempdir().unwrap();
    let shells = [
        ("bash", "complete -o filenames -F _uwe uwe", "--json"),
        ("zsh", "compdef _uwe uwe", "--json"),
        ("fish", "complete -c uwe", "-o json -l json"),
    ];
    for (shell, mark, option) in shells {
        let (code, script, errors) = uwe(root.path(), &["completions", shell]);
        assert_eq!(code, 0, "{errors}");
        assert!(
            script.contains(mark) && script.contains(option),
            "{shell}: {script}"
        );
        assert!(
            script.contains("serve") && script.contains("suggest"),
            "{shell}"
        );
        shell_accepts(shell, &script);
    }
    let (code, _, errors) = uwe(root.path(), &["completions", "tcsh"]);
    assert_eq!(code, 2, "{errors}");
}
