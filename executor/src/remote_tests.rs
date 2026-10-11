//! Checks plans, the helper and collection of object-store staging.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use task_model::{FORMAT, OutputRule, Placement, TaskSpec};

use super::*;
use crate::{Staged, Staging};

fn spec() -> TaskSpec {
    TaskSpec {
        format: FORMAT,
        arguments: vec![
            "sh".into(),
            "-c".into(),
            "cat in.txt tree/a.txt > out.txt && mkdir -p out/deep && echo b > out/deep/b.txt \
             && cat /opt/data/c.txt > /opt/result.txt && ./tree/run.sh && read line < /dev/stdin \
             && echo \"$line\""
                .into(),
        ],
        namespace: String::new(),
        image: None,
        workdir: None,
        environment: BTreeMap::new(),
        inputs: Vec::new(),
        outputs: ["out.txt", "out", "/opt/result.txt"]
            .map(|pattern| OutputRule {
                pattern: pattern.into(),
            })
            .to_vec(),
        stdin: None,
        containers: Vec::new(),
        replaces_entrypoint: true,
        run_as: None,
    }
}

#[tokio::test]
async fn refuses_unplaceable_inputs() {
    let folder = tempfile::tempdir().unwrap();
    let stage = RemoteStage {
        objects: Store::local(folder.path()).unwrap(),
        expiry: Duration::from_secs(60),
        max_outputs: 1,
    };
    let spec = spec();
    let placement = Placement::default();
    for input in [
        Staged {
            path: "../x".into(),
            source: folder.path().to_owned(),
            ..Staged::default()
        },
        Staged {
            path: "x".into(),
            source: folder.path().to_owned(),
            in_place: true,
            ..Staged::default()
        },
    ] {
        let staging = Staging {
            inputs: vec![input],
            ..Staging::default()
        };
        let job = Job {
            name: "refused",
            spec: &spec,
            staging: &staging,
            placement: &placement,
        };
        let error = stage.upload(job, folder.path(), &[]).await.unwrap_err();
        assert!(matches!(error, ExecutorError::Unsupported(_)), "{error}");
    }
}
