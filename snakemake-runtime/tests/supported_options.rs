//! Unsupported behavior-changing options fail before a task can start.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use workflow_graph::Launch;

#[test]
fn unsupported_rule_options_reject_enabled_values_and_preserve_disabled_values() {
    for option in ["shadow", "cache", "handover"] {
        for value in ["True", "'minimal'", "False", "None", "''"] {
            let folder = tempfile::tempdir().expect("folder");
            std::fs::write(
                folder.path().join("Snakefile"),
                format!(
                    "rule make:\n    output: 'out.txt'\n    {option}: {value}\n    shell: 'echo made > {{output}}'\n"
                ),
            )
            .expect("Snakefile");
            let launch = Launch {
                target: folder.path().to_path_buf(),
                scratch: folder.path().join("scratch"),
                ..Launch::default()
            };
            let result = snakemake_runtime::driver(&launch).and_then(|mut driver| driver.start());
            if matches!(value, "True" | "'minimal'") {
                let error = result.expect_err("enabled option must fail").to_string();
                assert!(
                    error.contains(option) && error.contains("not supported"),
                    "{error}"
                );
            } else {
                assert_eq!(result.expect("disabled option starts").len(), 1);
            }
        }
    }
}

#[test]
fn nonempty_resource_scopes_fail_at_dispatch() {
    let directive = "resource_scopes: mem_mb='global'";
    let folder = tempfile::tempdir().expect("folder");
    std::fs::write(
        folder.path().join("Snakefile"),
        format!(
            "{directive}\nrule make:\n    output: 'out.txt'\n    shell: 'echo made > {{output}}'\n"
        ),
    )
    .expect("Snakefile");
    let launch = Launch {
        target: folder.path().to_path_buf(),
        scratch: folder.path().join("scratch"),
        ..Launch::default()
    };
    let error = snakemake_runtime::driver(&launch)
        .and_then(|mut driver| driver.start())
        .expect_err("unsupported directive")
        .to_string();
    assert!(error.contains("not supported"), "{error}");
}

#[test]
fn empty_resource_scopes_preserve_default_scheduling() {
    for value in ["{}", "**{}"] {
        let folder = tempfile::tempdir().expect("folder");
        std::fs::write(
            folder.path().join("Snakefile"),
            format!("resource_scopes: {value}\nrule make:\n    output: 'out.txt'\n    shell: 'echo made > {{output}}'\n"),
        )
        .expect("Snakefile");
        let launch = Launch {
            target: folder.path().to_path_buf(),
            scratch: folder.path().join("scratch"),
            ..Launch::default()
        };
        let tasks = snakemake_runtime::driver(&launch)
            .and_then(|mut driver| driver.start())
            .expect("empty scopes");
        assert_eq!(tasks.len(), 1);
    }
}
