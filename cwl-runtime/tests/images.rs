//! Checks CWL image selection, content-addressed recipes and explicit remote refusal.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use cwl_runtime::{CwlDriver, Options};
use workflow_graph::{Driver, Launch};

fn image(root: &std::path::Path, fields: &str, options: Options) -> Result<String, String> {
    image_in(root, "requirements", fields, options)
}

/// The image of a tool whose DockerRequirement is in `section`, `requirements` or `hints`; an
/// error of an unsupported feature starts with `unsupported: `.
fn image_in(
    root: &std::path::Path,
    section: &str,
    fields: &str,
    options: Options,
) -> Result<String, String> {
    std::fs::write(
        root.join("main.cwl"),
        format!(
            "cwlVersion: v1.2\nclass: CommandLineTool\n\
        baseCommand: echo\n{section}:\n  DockerRequirement:\n{fields}\ninputs: []\noutputs: []\n"
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
    let mut tasks = driver.start().map_err(|error| match error.unsupported {
        true => format!("unsupported: {}", error.message),
        false => error.message,
    })?;
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
    let fields = [
        "    dockerFile: 'FROM scratch'",
        "    dockerLoad: missing.tar",
        "    dockerImport: missing.tar",
    ];
    // As in cwltool, a hint fails too, rather than run in the default image, but only a
    // requirement is unsupported.
    for (section, fields) in ["requirements", "hints"]
        .into_iter()
        .flat_map(|section| fields.map(|fields| (section, fields)))
    {
        let remote = Options {
            simulated: false,
            ..Options::default()
        };
        let error = image_in(root.path(), section, fields, remote).unwrap_err();
        assert!(error.contains("`[cwl]` section"), "{error}");
        assert!(error.contains("Apptainer on SLURM"), "{error}");
        let unsupported = error.starts_with("unsupported: ");
        assert_eq!(unsupported, section == "requirements", "{error}");
    }
}

#[test]
#[ignore = "needs Docker and a registry at IUNCTUS_TEST_REGISTRY, like localhost:5000/cwl"]
fn pushes_remote_images() {
    let registry = std::env::var("IUNCTUS_TEST_REGISTRY").expect("IUNCTUS_TEST_REGISTRY");
    let root = tempfile::tempdir().unwrap();
    let remote = Options {
        simulated: false,
        registry: Some(registry.clone()),
        ..Options::default()
    };
    let fields = "    dockerFile: \"FROM scratch\\nLABEL org.iunctus.test=push\"";
    let pushed = image_in(root.path(), "hints", fields, remote).unwrap();
    assert!(
        pushed.starts_with(&format!("{registry}@sha256:")),
        "{pushed}"
    );
}
