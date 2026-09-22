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
