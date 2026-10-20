//! Unsupported behavior-changing options fail before a task can start.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use workflow_graph::Launch;

#[test]
fn rejects_active_options() {
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
            // Snakemake 9.27 takes `shadow: True` as the full shadow.
            let supported = option == "shadow" || (option == "cache" && value == "True");
            if matches!(value, "True" | "'minimal'") && !supported {
                let error = result.expect_err("unsupported option must fail");
                assert!(error.message.contains(option), "{error}");
                if option != "handover" {
                    assert!(error.unsupported, "{error}");
                }
            } else {
                assert_eq!(result.expect("disabled option starts").len(), 1);
            }
        }
    }
}

/// Snakemake takes `resource_scopes:` and refuses scopes other than local, global and
/// excluded; a local run tallies every resource whatever its scope.
#[test]
fn checks_resource_scopes() {
    for (scopes, valid) in [
        ("mem_mb='global', fake_res='local'", true),
        ("mem_mb='node'", false),
    ] {
        let folder = tempfile::tempdir().expect("folder");
        std::fs::write(
            folder.path().join("Snakefile"),
            format!("resource_scopes:\n    {scopes}\nrule make:\n    output: 'out.txt'\n    shell: 'echo made > {{output}}'\n"),
        )
        .expect("Snakefile");
        let launch = Launch {
            target: folder.path().to_path_buf(),
            scratch: folder.path().join("scratch"),
            ..Launch::default()
        };
        let result = snakemake_runtime::driver(&launch).and_then(|mut driver| driver.start());
        match valid {
            true => assert_eq!(result.expect("valid scopes").len(), 1),
            false => {
                let error = result.expect_err("an invalid scope").to_string();
                assert!(error.contains("Invalid scope"), "{error}");
            }
        }
    }
}

#[test]
fn preserves_default_scheduling() {
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

#[test]
fn stages_minimal_shadow() {
    for mode in ["'minimal'", "False"] {
        let folder = tempfile::tempdir().expect("a folder");
        std::fs::write(folder.path().join("data"), "input").expect("input data");
        std::fs::write(folder.path().join("config.yaml"), "value: 1\n").expect("config");
        std::fs::write(
            folder.path().join("Snakefile"),
            format!("configfile: 'config.yaml'\nrule make:\n    input: 'data'\n    output: 'result/out'\n    log: 'logs/make.log'\n    shadow: {mode}\n    cache: True\n    shell: 'cat {{input}} > {{output}}'\n"),
        ).expect("Snakefile");
        let launch = Launch {
            target: folder.path().to_path_buf(),
            scratch: folder.path().join("scratch"),
            ..Launch::default()
        };
        let task = snakemake_runtime::driver(&launch)
            .unwrap()
            .start()
            .unwrap()
            .remove(0);
        assert!(task.workdir.is_none());
        assert!(task.inputs.iter().any(|input| input.path == "data"));
        assert_eq!(
            task.inputs.iter().any(|input| input.path == "config.yaml"),
            mode == "False"
        );
        assert!(
            task.inputs
                .iter()
                .any(|input| input.path == "result" && input.directory)
        );
        assert!(
            task.inputs
                .iter()
                .any(|input| input.path == "logs" && input.directory)
        );
        assert_eq!(task.reuse, workflow_graph::Reuse::Allowed);
    }
}

#[test]
fn rejects_shadow_modes() {
    for directive in ["shadow: 'deep'", "shadow: 'minimal'\n    output: '../out'"] {
        let folder = tempfile::tempdir().expect("a folder");
        std::fs::write(
            folder.path().join("Snakefile"),
            format!("rule make:\n    {directive}\n    shell: 'true'\n"),
        )
        .expect("Snakefile");
        let launch = Launch {
            target: folder.path().to_path_buf(),
            scratch: folder.path().join("scratch"),
            ..Launch::default()
        };
        let error = snakemake_runtime::driver(&launch)
            .unwrap()
            .start()
            .expect_err("refused");
        assert!(!error.unsupported, "{error}");
    }
}

#[test]
fn stages_shadow_links() {
    for mode in ["shallow", "full"] {
        let folder = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(folder.path().join("data/empty")).unwrap();
        std::fs::create_dir_all(folder.path().join(".snakemake/metadata")).unwrap();
        std::fs::write(folder.path().join("data/value"), "data").unwrap();
        std::fs::write(folder.path().join("extra"), "extra").unwrap();
        std::fs::write(folder.path().join("Snakefile"), format!(
            "rule make:\n    input: 'data/value'\n    output: 'out'\n    shadow: '{mode}'\n    shell: 'cat {{input}} extra > {{output}}'\n"
        )).unwrap();
        let launch = Launch {
            target: folder.path().into(),
            scratch: folder.path().join("scratch"),
            ..Launch::default()
        };
        let task = snakemake_runtime::driver(&launch)
            .unwrap()
            .start()
            .unwrap()
            .remove(0);
        assert!(task.inputs.iter().any(|input| input.path == "extra"));
        assert!(task.inputs.iter().any(|input| input.path == "Snakefile"));
        assert!(
            !task
                .inputs
                .iter()
                .any(|input| input.path.starts_with(".snakemake/metadata"))
        );
        if mode == "shallow" {
            assert!(task.command[2].contains("ln -s --"));
            assert!(
                task.inputs
                    .iter()
                    .any(|input| input.path == folder.path().join("data").to_string_lossy())
            );
        } else {
            assert!(
                task.inputs
                    .iter()
                    .any(|input| input.path == "data/empty" && input.source.is_empty())
            );
        }
    }
}

#[test]
fn confines_shadow_outputs() {
    for (mode, output) in [
        ("shallow", "data/out"),
        ("full", "/tmp/out"),
        ("full", "../out"),
    ] {
        let folder = tempfile::tempdir().unwrap();
        std::fs::create_dir(folder.path().join("data")).unwrap();
        std::fs::write(
            folder.path().join("Snakefile"),
            format!(
                "rule make:\n    output: '{output}'\n    shadow: '{mode}'\n    shell: 'true'\n"
            ),
        )
        .unwrap();
        let launch = Launch {
            target: folder.path().into(),
            scratch: folder.path().join("scratch"),
            ..Launch::default()
        };
        assert!(snakemake_runtime::driver(&launch).unwrap().start().is_err());
    }
}

/// A `copy-minimal` shadow job gets copies of its inputs, not links (test_shadow_copy).
#[test]
fn copies_minimal_inputs() {
    let folder = tempfile::tempdir().expect("a folder");
    std::fs::write(folder.path().join("in.txt"), "x").expect("an input");
    std::fs::write(
        folder.path().join("Snakefile"),
        "rule make:\n    input: 'in.txt'\n    output: 'out.txt'\n    shadow: 'copy-minimal'\n    shell: 'cp {input} {output}'\n",
    )
    .expect("Snakefile");
    let launch = Launch {
        target: folder.path().to_path_buf(),
        scratch: folder.path().join("scratch"),
        ..Launch::default()
    };
    let task = snakemake_runtime::driver(&launch)
        .unwrap()
        .start()
        .unwrap()
        .remove(0);
    let input = task
        .inputs
        .iter()
        .find(|input| input.path == "in.txt")
        .unwrap();
    assert_eq!(input.writable, workflow_graph::Writable::Copy);
}
