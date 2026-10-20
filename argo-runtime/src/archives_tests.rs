//! Verifies archive bytes, retained native files and temporary folder cleanup.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fs::File;
use std::io::Read;
use std::rc::Rc;

use workflow_graph::{Driver, Outcome, OutputFile};

use crate::{
    ArgoDriver, Options,
    library::{Document, Kind, Library},
};

fn driver(strategy: &str, s3: bool) -> ArgoDriver {
    let repository = if s3 {
        ", s3: {bucket: data, key: exported}"
    } else {
        ""
    };
    let source = format!(
        "kind: Workflow\nmetadata: {{name: archive}}\nspec:\n  entrypoint: main\n  templates:\n  - name: main\n    container: {{image: debian:stable-slim}}\n    outputs:\n      artifacts: [{{name: tree, path: /tree, archive: {strategy}{repository}}}]\n"
    );
    let document = Rc::new(Document {
        path: "archive.yaml".into(),
        index: 0,
        kind: Kind::Workflow,
        name: "archive".into(),
        node: argo_parser::parse_yaml(&source).unwrap().unwrap(),
    });
    ArgoDriver::new(document, Rc::new(Library::new()), Options::default())
}

#[test]
fn exports_archive_bytes() {
    let folder = tempfile::tempdir().unwrap();
    let tree = folder.path().join("tree");
    std::fs::create_dir(&tree).unwrap();
    std::fs::write(tree.join("input"), "content").unwrap();
    for strategy in [
        "{tar: {compressionLevel: 0}}",
        "{tar: {compressionLevel: 9}}",
        "{zip: {}}",
        "{none: {}}",
    ] {
        let mut driver = driver(strategy, true);
        let task = driver.start().unwrap().remove(0);
        driver
            .complete(Outcome {
                id: task.id,
                files: vec![OutputFile {
                    path: "/tree".into(),
                    location: tree.to_string_lossy().into_owned(),
                    directory: true,
                    ..OutputFile::default()
                }],
                ..Outcome::default()
            })
            .unwrap();
        let deliveries = driver.deliveries();
        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].target, "s3://data/exported");
        let location = std::path::Path::new(&deliveries[0].location);
        let mut content = String::new();
        if strategy.contains("tar") {
            let mut tar =
                tar::Archive::new(flate2::read::GzDecoder::new(File::open(location).unwrap()));
            let mut found = false;
            for entry in tar.entries().unwrap() {
                let mut entry = entry.unwrap();
                if entry.path().unwrap() == std::path::Path::new("tree/input") {
                    entry.read_to_string(&mut content).unwrap();
                    found = true;
                }
            }
            assert!(found);
        } else if strategy.contains("zip") {
            let mut zip = zip::ZipArchive::new(File::open(location).unwrap()).unwrap();
            zip.by_name("tree/input")
                .unwrap()
                .read_to_string(&mut content)
                .unwrap();
        } else {
            content = std::fs::read_to_string(location.join("input")).unwrap();
        }
        assert_eq!(content, "content");
        let run = driver.run.as_ref().unwrap();
        assert_eq!(
            run.nodes[0].outputs.artifacts[0].location,
            Some(crate::node::Location::File(tree.clone()))
        );
        assert!(driver.finish().is_ok());
        let temporary = run_folder(&driver);
        drop(driver);
        if let Some(temporary) = temporary {
            assert!(!temporary.exists());
        }
    }
}

fn run_folder(driver: &ArgoDriver) -> Option<std::path::PathBuf> {
    driver
        .run
        .as_ref()
        .unwrap()
        .archives
        .as_ref()
        .map(|folder| folder.path().to_owned())
}

#[test]
fn refuses_huffman_compression() {
    let error = driver("{tar: {compressionLevel: -2}}", false)
        .start()
        .unwrap_err();
    assert!(error.unsupported);
    assert!(error.message.contains("Huffman-only"));
}

#[test]
fn rejects_multiple_strategies() {
    assert!(
        driver("{none: {}, zip: {}}", false)
            .start()
            .unwrap_err()
            .message
            .contains("one strategy")
    );
}

#[test]
fn downstream_keeps_files() {
    let folder = tempfile::tempdir().unwrap();
    let file = folder.path().join("input");
    std::fs::write(&file, "original").unwrap();
    let source = "kind: Workflow\nmetadata: {name: native}\nspec:\n  entrypoint: main\n  templates:\n  - name: main\n    steps:\n    - - {name: make, template: make}\n    - - name: use\n        template: use\n        arguments: {artifacts: [{name: file, from: '{{steps.make.outputs.artifacts.file}}'}]}\n  - name: make\n    container: {image: debian:stable-slim}\n    outputs: {artifacts: [{name: file, path: /input, archive: {tar: {}}}]}\n  - name: use\n    container: {image: debian:stable-slim}\n    inputs: {artifacts: [{name: file, path: /input, archive: {tar: {}}}]}\n";
    let document = Rc::new(Document {
        path: "native.yaml".into(),
        index: 0,
        kind: Kind::Workflow,
        name: "native".into(),
        node: argo_parser::parse_yaml(source).unwrap().unwrap(),
    });
    let mut driver = ArgoDriver::new(document, Rc::new(Library::new()), Options::default());
    let task = driver.start().unwrap().remove(0);
    let next = driver
        .complete(Outcome {
            id: task.id,
            files: vec![OutputFile {
                path: "/input".into(),
                location: file.to_string_lossy().into_owned(),
                ..OutputFile::default()
            }],
            ..Outcome::default()
        })
        .unwrap();
    assert_eq!(next[0].inputs[0].source, file.to_string_lossy());
    assert_eq!(
        std::fs::read_to_string(&next[0].inputs[0].source).unwrap(),
        "original"
    );
}

#[test]
fn requires_repository_location() {
    let artifact = |source: &str| argo_parser::model::Artifact {
        name: "output".into(),
        s3: argo_parser::parse_yaml(source).unwrap(),
        ..Default::default()
    };
    // A key alone names the artifact repository of the run.
    assert!(super::check(&artifact("{key: data}")).is_ok());
    let error = super::check(&artifact("{bucket: data}")).unwrap_err();
    assert!(error.unsupported);
    assert!(error.message.contains("supply a key"), "{}", error.message);
}
