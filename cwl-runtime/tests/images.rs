//! Checks CWL image selection, content-addressed recipes and explicit remote refusal.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use cwl_runtime::{CwlDriver, Options};
use workflow_graph::{Driver, Launch};

fn image(root: &std::path::Path, fields: &str, options: Options) -> Result<String, String> {
    std::fs::write(
        root.join("main.cwl"),
        format!(
            "cwlVersion: v1.2\nclass: CommandLineTool\n\
        baseCommand: echo\nrequirements:\n  DockerRequirement:\n{fields}\ninputs: []\noutputs: []\n"
        ),
    )
    .unwrap();
    let mut driver = CwlDriver::new(
        &Launch {
            target: root.join("main.cwl"),
            ..Launch::default()
        },
        options,
    )
    .map_err(|error| error.message)?;
    let mut tasks = driver.start().map_err(|error| error.message)?;
    Ok(tasks.remove(0).image.unwrap())
}

#[test]
fn hashes_build_context() {
    let root = tempfile::tempdir().unwrap();
    let fields = "    dockerFile: 'FROM scratch'\n    dockerImageId: declared:tag\n    dockerContext: context";
    std::fs::create_dir(root.path().join("context")).unwrap();
    std::fs::write(root.path().join("context/seed"), "one").unwrap();
    let first = image(root.path(), fields, Options::default()).unwrap();
    assert!(first.starts_with("iunctus-cwl:build-"));
    assert_eq!(
        image(root.path(), fields, Options::default()).unwrap(),
        first
    );
    std::fs::write(root.path().join("context/seed"), "two").unwrap();
    assert_ne!(
        image(root.path(), fields, Options::default()).unwrap(),
        first
    );
}

#[test]
fn hashes_image_archives() {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("image.tar");
    for field in ["dockerLoad", "dockerImport"] {
        std::fs::write(&archive, "one").unwrap();
        let fields = format!("    {field}: image.tar\n    dockerImageId: declared:tag");
        let first = image(root.path(), &fields, Options::default()).unwrap();
        assert!(first.starts_with("iunctus-cwl:"));
        assert_eq!(
            image(root.path(), &fields, Options::default()).unwrap(),
            first
        );
        std::fs::write(&archive, "two").unwrap();
        assert_ne!(
            image(root.path(), &fields, Options::default()).unwrap(),
            first
        );
    }
}

#[test]
fn refuses_remote_preparation() {
    let root = tempfile::tempdir().unwrap();
    for fields in [
        "    dockerFile: 'FROM scratch'",
        "    dockerLoad: missing.tar",
        "    dockerImport: missing.tar",
    ] {
        let error = image(
            root.path(),
            fields,
            Options {
                simulated: false,
                ..Options::default()
            },
        )
        .unwrap_err();
        assert!(error.contains("local task executor"), "{error}");
        assert!(error.contains("registry push"), "{error}");
    }
}
