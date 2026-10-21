//! Checks site configuration selection and isolation from user configuration.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;
use std::process::Command;

mod common;

const SCRIPT: &str = "workflow { }\n";

/// A config file with a section that uwe does not know, so that reading it is an error.
const BAD_CONFIG: &str = "[nonsense]\nkey = 1\n";

/// Prints the graph of an empty workflow in `directory`, with the exit code and standard error.
fn graph(command: &mut Command, directory: &Path) -> (i32, String) {
    std::fs::write(directory.join("main.nf"), SCRIPT).expect("writes the script");
    let output = command
        .args(["graph", "main.nf"])
        .current_dir(directory)
        .env("XDG_CACHE_HOME", directory.join("cache"))
        .output()
        .expect("runs uwe");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn environment_selects_config() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let site = directory.path().join("site.toml");
    std::fs::write(&site, BAD_CONFIG).expect("writes the site file");
    let (code, error) = graph(
        common::command().env(config_file::SITE_FILE_VARIABLE, &site),
        directory.path(),
    );
    assert_ne!(code, 0, "{error}");
    assert!(error.contains(&site.display().to_string()), "{error}");
}

#[test]
fn isolates_user_configuration() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    // The user's file of a home folder, which the empty config folder makes unreachable.
    let user = directory.path().join("home/.config/uwe");
    std::fs::create_dir_all(&user).expect("makes the user's folder");
    std::fs::write(user.join("config.toml"), BAD_CONFIG).expect("writes the user's file");
    let (code, error) = graph(
        common::command().env("HOME", directory.path().join("home")),
        directory.path(),
    );
    assert_eq!(code, 0, "{error}");
}

#[test]
fn cache_configuration_wins() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let folder = directory.path().join("elsewhere");
    let site = directory.path().join("site.toml");
    // An executor that this machine cannot reach, which `uwe graph` must not use.
    let text = format!(
        "[executor]\ndefault = \"kubernetes\"\n\n[cache]\nfolder = \"{}\"\n",
        folder.display()
    );
    std::fs::write(&site, text).expect("writes the site file");
    let (code, error) = graph(
        common::command().env(config_file::SITE_FILE_VARIABLE, &site),
        directory.path(),
    );
    assert_eq!(code, 0, "{error}");
    assert!(folder.join(run_database::FILE_NAME).exists(), "{error}");
    assert!(!directory.path().join("cache/uwe").exists(), "{error}");
}

#[test]
fn executor_config_precedence() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("main.nf"), SCRIPT).unwrap();
    let config = directory.path().join("config.toml");
    for (default, option, code) in [
        ("simulator", None, 0),
        ("simulator", Some("local"), 3),
        ("local", Some("simulator"), 0),
    ] {
        std::fs::write(&config, format!("[executor]\ndefault = '{default}'\n")).unwrap();
        let mut command = common::command();
        command
            .args(["run", "main.nf"])
            .arg("-uwe-config")
            .arg(&config)
            .env("UWE_ENGINE", directory.path().join("missing.sock"))
            .env("XDG_CACHE_HOME", directory.path().join("cache"))
            .current_dir(directory.path());
        if let Some(option) = option {
            command.args(["-executor", option]);
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(code), "{output:?}");
        if code == 0 {
            assert!(String::from_utf8_lossy(&output.stdout).contains("outputs are made-up files"));
        }
    }
}

#[test]
fn inspection_uses_simulation() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("tool.cwl"),
        "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: echo\ninputs:\n  message: string\noutputs: []\n",
    ).unwrap();
    let config = directory.path().join("config.toml");
    std::fs::write(&config, "[executor]\ndefault = 'local'\n").unwrap();
    for name in ["graph", "check", "check-order", "inputs", "outputs"] {
        let output = common::command()
            .args([name, "tool.cwl"])
            .arg("-uwe-config")
            .arg(&config)
            .env("UWE_ENGINE", directory.path().join("missing.sock"))
            .env("XDG_CACHE_HOME", directory.path().join("cache"))
            .current_dir(directory.path())
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(if name == "check" { 1 } else { 0 }),
            "{name}: {output:?}"
        );
        assert!(!String::from_utf8_lossy(&output.stderr).contains("cannot reach"));
    }
}
