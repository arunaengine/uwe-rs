//! Input snapshots use immutable references and retain executable and directory inputs.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::Read;

use aruna_executor::bundle::prepare;
use executor::{Job, Staged, Staging};
use task_model::{Image, Placement, TaskSpec};

#[test]
fn snapshots_include_inputs_stdin_and_a_result_bundle_command() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("file"), b"input").unwrap();
    let spec = TaskSpec {
        image: Some(Image {
            reference: "image".into(),
            platform: None,
        }),
        arguments: vec!["true".into()],
        format: task_model::FORMAT,
        namespace: "test".into(),
        workdir: None,
        environment: Default::default(),
        inputs: Vec::new(),
        outputs: Vec::new(),
        stdin: None,
        containers: Vec::new(),
        replaces_entrypoint: true,
        run_as: None,
    };
    let staging = Staging {
        inputs: vec![Staged {
            path: "nested".into(),
            source: source.clone(),
            directory: true,
            executable: true,
            copy: true,
            ..Staged::default()
        }],
        directories: vec!["empty".into()],
        stdin: Some(source.join("file")),
    };
    let placement = Placement::default();
    let job = Job {
        name: "job",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let path = prepare(job, root.path()).unwrap();
    let mut archive = tar::Archive::new(std::fs::File::open(path).unwrap());
    let mut entries = std::collections::BTreeMap::new();
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        let path = entry.path().unwrap().to_string_lossy().into_owned();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        entries.insert(path, bytes);
    }
    assert_eq!(entries["work/nested/file"], b"input");
    assert_eq!(entries["control/stdin"], b"input");
    let start = std::str::from_utf8(&entries["start.sh"]).unwrap();
    assert!(start.contains("chmod u+x '/iunctus/work/nested'"));
    assert!(start.contains("mkdir -p '/iunctus/work/empty'"));
    assert!(start.contains("tar -chf /iunctus/result.tar -C /iunctus control outbox"));
    let mut inplace = staging.clone();
    inplace.inputs[0].in_place = true;
    assert!(
        prepare(
            Job {
                staging: &inplace,
                ..job
            },
            root.path()
        )
        .is_err()
    );
}
