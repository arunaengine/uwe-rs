//! Checks plans, the helper and collection together, with a staging root in a folder or in S3.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::PathBuf;

use task_model::{FORMAT, OutputRule, Placement, TaskSpec};

use super::*;
use crate::capture::RECORDS;
use crate::wrapper::{Folders, wrapper};
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

/// Inputs of every kind: a file, a directory with a program and an empty folder, an absolute
/// file, and standard input.
fn staging(sources: &Path) -> Staging {
    std::fs::create_dir_all(sources.join("tree/empty")).unwrap();
    std::fs::write(sources.join("in.txt"), "a\n").unwrap();
    std::fs::write(sources.join("tree/a.txt"), "tree\n").unwrap();
    std::fs::write(sources.join("tree/run.sh"), "#!/bin/sh\necho ran\n").unwrap();
    let mode = std::fs::Permissions::from_mode(0o755);
    std::fs::set_permissions(sources.join("tree/run.sh"), mode).unwrap();
    std::fs::write(sources.join("c.txt"), "c\n").unwrap();
    std::fs::write(sources.join("stdin"), "line\n").unwrap();
    let staged = |path: &str, source: &str, directory| Staged {
        path: path.into(),
        source: sources.join(source),
        directory,
        ..Staged::default()
    };
    Staging {
        inputs: vec![
            staged("in.txt", "in.txt", false),
            staged("tree", "tree", true),
            staged("/opt/data/c.txt", "c.txt", false),
        ],
        directories: vec!["made".into()],
        stdin: Some(sources.join("stdin")),
    }
}

/// Plans an attempt, runs its captured wrapper in a node folder between stage-in and stage-out,
/// and collects it into a scratch folder on this machine.
async fn round_trip(stage: &RemoteStage) {
    let sources = tempfile::tempdir().unwrap();
    let node = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let mut spec = spec();
    // The node folder stands in for where the container sees the scratch folder.
    let root = node.path().join("attempt");
    spec.stdin = Some(Digest::sha256(b"line\n"));
    let staging = staging(sources.path());
    let placement = Placement::default();
    let job = Job {
        name: "remote-1",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let text = |path: PathBuf| path.to_string_lossy().into_owned();
    let (work, control) = (text(root.join(WORK)), text(root.join(CONTROL)));
    let (home, outbox) = (text(root.join(HOME)), text(root.join("outbox")));
    let folders = Folders {
        work: &work,
        control: &control,
        home: &home,
        outbox: &outbox,
        owner: "",
    };
    // Absolute outputs go to the copies in the control folder, as in a container.
    let script =
        wrapper(&spec, &folders).replace("/opt/", &format!("{}/mounts/opt/", text(root.clone())));
    let script = crate::capture::capture(&script, &text(root.join("transfer"))).unwrap();
    let extra = [("control/wrapper.sh", script)];
    let planned = stage
        .plan(job, &root, &extra, RECORDS, false)
        .await
        .unwrap();
    assert_eq!(planned.mounts.len(), 1);
    assert_eq!(planned.mounts[0].target, "/opt/data/c.txt");
    assert_eq!(planned.mounts[0].source, root.join("mounts/opt/data/c.txt"));
    let (folder, location) = (root.clone(), planned.location.clone());
    tokio::task::spawn_blocking(move || {
        let folder = text(folder);
        crate::helper::main(&["in".into(), folder.clone(), location]).unwrap();
        let run = std::process::Command::new("sh")
            .arg(format!("{folder}/control/wrapper.sh"))
            .output()
            .unwrap();
        assert!(run.status.success(), "{run:?}");
        crate::helper::main(&["out".into(), folder]).unwrap();
    })
    .await
    .unwrap();
    assert!(root.join("work/made").is_dir());
    assert!(root.join("work/tree/empty").is_dir());
    let host = scratch.path().join("attempt");
    stage
        .collect(&planned.prefix, &host, RECORDS)
        .await
        .unwrap();
    let finished = Finished::from_control(host.clone()).unwrap();
    assert_eq!(finished.exit_code, Some(0));
    let read = |path: &str| std::fs::read_to_string(finished.local_path(path)).unwrap();
    assert_eq!(read("out.txt"), "a\ntree\n");
    assert_eq!(read("out/deep/b.txt"), "b\n");
    assert_eq!(
        std::fs::read_to_string(finished.stdout()).unwrap(),
        "ran\nline\n"
    );
    let absolute = format!("{}/mounts/opt/result.txt", text(root.clone()));
    assert_eq!(read(&absolute), "c\n");
}

#[tokio::test]
async fn folder_round_trip() {
    let folder = tempfile::tempdir().unwrap();
    let stage = RemoteStage {
        objects: Store::local(folder.path()).unwrap(),
        expiry: Duration::from_secs(60),
        max_outputs: 8,
        credentials: false,
    };
    round_trip(&stage).await;
}

#[tokio::test]
async fn refuses_unplaceable_inputs() {
    let folder = tempfile::tempdir().unwrap();
    let stage = RemoteStage {
        objects: Store::local(folder.path()).unwrap(),
        expiry: Duration::from_secs(60),
        max_outputs: 1,
        credentials: false,
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

/// A staging root in the S3 bucket of the environment, below its own prefix.
fn s3_stage(name: &str, credentials: bool) -> RemoteStage {
    let options = content_store::S3Options {
        bucket: std::env::var("IUNCTUS_S3_BUCKET").expect("IUNCTUS_S3_BUCKET is set"),
        prefix: Some(format!("iunctus-{name}-{}", std::process::id())),
        endpoint: std::env::var("IUNCTUS_S3_ENDPOINT").ok(),
        region: Some(std::env::var("AWS_REGION").unwrap_or_else(|_| "us-east-1".into())),
    };
    RemoteStage {
        objects: Store::s3(&options).unwrap(),
        expiry: Duration::from_secs(600),
        max_outputs: 8,
        credentials,
    }
}

#[tokio::test]
#[ignore = "needs an S3 bucket; set IUNCTUS_S3_BUCKET and AWS credentials, then run with --ignored"]
async fn presigned_round_trip() {
    round_trip(&s3_stage("remote", false)).await;
}

#[tokio::test]
#[ignore = "needs an S3 bucket and its provider in IUNCTUS_STAGING_STORAGE; run `just s3-test`"]
async fn credentials_round_trip() {
    round_trip(&s3_stage("credentials", true)).await;
}

/// Sends an output larger than one part as an object in parts, which S3 marks with the count of
/// its parts in the ETag.
#[tokio::test]
#[ignore = "needs an S3 bucket and its provider in IUNCTUS_STAGING_STORAGE; run `just s3-test`"]
async fn sends_large_outputs() {
    let stage = s3_stage("large", true);
    let folder = tempfile::tempdir().unwrap();
    let source = folder.path().join("transfer/1");
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    let mut file = std::io::BufWriter::new(std::fs::File::create(&source).unwrap());
    for index in 0u32..(300 << 14) {
        std::io::Write::write_all(&mut file, &index.to_le_bytes().repeat(16)).unwrap();
    }
    drop(file);
    let request = stage.request("attempts/large/files/1", true).await.unwrap();
    assert!(request.url.starts_with("s3://"), "{}", request.url);
    let plan = Plan {
        files: vec![request.clone()],
        ..Plan::default()
    };
    let path = folder.path().to_owned();
    (tokio::task::spawn_blocking(move || crate::helper::stage_out(&path, &plan)).await)
        .unwrap()
        .unwrap();
    let back = folder.path().join("back");
    let read = stage
        .objects
        .read_object("attempts/large/files/1", None, &back, None);
    assert_eq!(read.await.unwrap(), hash_file(&source).await.unwrap());
    let text = std::env::var(crate::helper::STORAGE).unwrap();
    let config = config_file::ConfigFile::parse(&text, Path::new("storage.toml")).unwrap();
    let storage = storage::Storage::from_config(&config).unwrap();
    let metadata = storage.stat(&request.url).await.unwrap().unwrap();
    assert_eq!(metadata.size, 300 << 20);
    let tag = metadata.version.unwrap();
    assert!(tag.trim_matches('"').ends_with("-38"), "{tag}");
}
