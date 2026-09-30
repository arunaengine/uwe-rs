//! Profile retries reach job execution, and unsupported settings fail explicitly.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use workflow_graph::{Launch, Outcome};

#[test]
fn applies_retry_aliases() {
    for setting in ["retries", "restart-times"] {
        for explicit in ["", "    retries: 0\n", "    retries: None\n"] {
            let folder = tempfile::tempdir().expect("a folder");
            let profile = folder.path().join("profiles/default");
            std::fs::create_dir_all(&profile).expect("a profile folder");
            std::fs::write(profile.join("config.yaml"), format!("{setting}: 2\n"))
                .expect("a profile");
            std::fs::write(
                folder.path().join("Snakefile"),
                format!("rule make:\n    output: 'out'\n{explicit}    shell: 'false'\n"),
            )
            .expect("a Snakefile");
            let launch = Launch {
                target: folder.path().to_path_buf(),
                scratch: folder.path().join("scratch"),
                ..Launch::default()
            };
            let mut driver = snakemake_runtime::driver(&launch).expect("a driver");
            let task = driver.start().expect("starts").remove(0);
            let attempts = if explicit.contains('0') { 1 } else { 3 };
            for attempt in 1..=attempts {
                let next = driver
                    .complete(Outcome {
                        id: task.id,
                        attempt,
                        exit_code: 1,
                        ..Outcome::default()
                    })
                    .expect("handles a failed attempt");
                assert_eq!(next.len(), usize::from(attempt < attempts));
                if let Some(retry) = next.first() {
                    assert_eq!(retry.id, task.id);
                    assert_eq!(retry.reuse, workflow_graph::Reuse::Denied);
                }
            }
            assert!(driver.finish().is_err());
        }
    }
}

#[test]
fn diagnoses_profile_settings() {
    for (settings, unsupported) in [
        ("notemp: true\n", true),
        ("no-hooks: true\n", true),
        ("retries: -1\n", false),
        ("restart-times: two\n", false),
        ("retries: 4294967296\n", false),
    ] {
        let folder = tempfile::tempdir().expect("a folder");
        let profile = folder.path().join("profiles/default");
        std::fs::create_dir_all(&profile).expect("a profile folder");
        std::fs::write(profile.join("config.yaml"), settings).expect("a profile");
        std::fs::write(
            folder.path().join("Snakefile"),
            "rule make:\n    shell: 'true'\n",
        )
        .expect("a Snakefile");
        let launch = Launch {
            target: folder.path().to_path_buf(),
            scratch: folder.path().join("scratch"),
            ..Launch::default()
        };
        let error = snakemake_runtime::driver(&launch)
            .expect("a driver")
            .start()
            .expect_err("invalid or unsupported setting");
        assert_eq!(error.unsupported, unsupported, "{error}");
        assert!(error.message.contains("rofile"), "{error}");
    }
}

#[test]
fn ignores_execution_settings() {
    let folder = tempfile::tempdir().expect("a folder");
    let profile = folder.path().join("profiles/default");
    std::fs::create_dir_all(&profile).expect("a profile folder");
    std::fs::write(
        profile.join("config.yaml"),
        "cores: 3\njobs: 8\nuse-conda: true\nsoftware-deployment-method: conda\nexecutor: slurm\nrerun-triggers: [mtime]\n",
    )
    .expect("a profile");
    std::fs::write(
        folder.path().join("Snakefile"),
        "rule make:\n    cache: 'omit-software'\n    output: 'out'\n    shell: 'touch out'\n",
    )
    .expect("a Snakefile");
    let launch = Launch {
        target: folder.path().to_path_buf(),
        scratch: folder.path().join("scratch"),
        ..Launch::default()
    };
    snakemake_runtime::driver(&launch)
        .expect("a driver")
        .start()
        .expect("ignored settings and omit-software caching");
}
