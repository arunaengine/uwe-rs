//! Checks plans, the helper and collection together, with a staging root in a folder or in S3.
// Copyright (c) 2026 The uwe Contributors
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
        output_manifest: None,
        format: FORMAT,
        arguments: vec![
            "sh".into(),
            "-c".into(),
            "cat in.txt tree/a.txt > out.txt && mkdir -p out/deep && echo b > out/deep/b.txt \
             && cp tree/run.sh out/run.sh && cat /opt/data/c.txt > /opt/result.txt && ./tree/run.sh && read line < /dev/stdin \
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
async fn round_trip(stage: &RemoteStage) -> Planned {
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
        .plan(job, &root, &extra, &[], RECORDS, false)
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
    // The outputs get the modes that a local task gives them.
    let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    std::fs::write(host.join("plain"), "plain").unwrap();
    let plain = mode(&host.join("plain"));
    assert_eq!(mode(&finished.local_path("out.txt")), plain);
    let program = mode(&finished.local_path("out/run.sh"));
    assert_eq!(
        program,
        plain | (plain & 0o444) >> 2,
        "a program stays executable"
    );
    assert_eq!(
        std::fs::read_to_string(finished.stdout()).unwrap(),
        "ran\nline\n"
    );
    let absolute = format!("{}/mounts/opt/result.txt", text(root.clone()));
    assert_eq!(read(&absolute), "c\n");
    planned
}

#[tokio::test]
async fn folder_round_trip() {
    let folder = tempfile::tempdir().unwrap();
    let stage = RemoteStage {
        objects: Store::local(folder.path()).unwrap(),
        expiry: Duration::from_secs(60),
        max_outputs: 8,
        credentials: false,
        cleanup: false,
    };
    round_trip(&stage).await;
}

#[tokio::test]
async fn cleans_attempts() {
    let folder = tempfile::tempdir().unwrap();
    let mut stage = RemoteStage {
        objects: Store::local(folder.path()).unwrap(),
        expiry: Duration::from_secs(60),
        max_outputs: 8,
        credentials: false,
        cleanup: false,
    };
    let planned = round_trip(&stage).await;
    let sent = format!("{}/", planned.prefix);
    stage.clean(&planned.prefix, &planned.plan).await;
    assert!(!stage.objects.objects(&sent).await.unwrap().is_empty());
    stage.cleanup = true;
    stage.clean(&planned.prefix, &planned.plan).await;
    assert!(stage.objects.objects(&sent).await.unwrap().is_empty());
    assert!(!folder.path().join(&planned.plan).exists());
    // The inputs stay for other attempts.
    let input = Digest::sha256(b"a\n");
    assert!(stage.objects.has_content(&input).await.unwrap());
}

/// A binary for the jobs in the folder `folder`.
fn binary(folder: &Path, bytes: &[u8]) -> Binary {
    let file = folder.join("uwe");
    std::fs::write(&file, bytes).unwrap();
    Binary {
        file,
        digest: Digest::sha256(bytes),
        machine: crate::binary::Machine::Amd64,
        uploaded: Default::default(),
    }
}

#[tokio::test]
async fn uploads_binary_once() {
    let (folder, sources) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let stage = RemoteStage {
        objects: Store::local(folder.path()).unwrap(),
        expiry: Duration::from_secs(60),
        max_outputs: 1,
        credentials: true,
        cleanup: false,
    };
    let binary = binary(sources.path(), b"static binary\n");
    let object = folder.path().join(layout::content(&binary.digest));
    let url = stage.binary(&binary).await.unwrap();
    assert_eq!(url, reqwest::Url::from_file_path(&object).unwrap().as_str());
    assert_eq!(std::fs::read(&object).unwrap(), b"static binary\n");
    // A later attempt neither checks nor uploads it again.
    std::fs::remove_file(&object).unwrap();
    stage.binary(&binary).await.unwrap();
    assert!(!object.exists());
    // Another process finds it in the staging root and does not read its file.
    let other = Binary {
        uploaded: Default::default(),
        ..binary.clone()
    };
    std::fs::write(&object, "static binary\n").unwrap();
    std::fs::remove_file(&other.file).unwrap();
    stage.binary(&other).await.unwrap();
}

#[tokio::test]
async fn refuses_unplaceable_inputs() {
    let folder = tempfile::tempdir().unwrap();
    let stage = RemoteStage {
        objects: Store::local(folder.path()).unwrap(),
        expiry: Duration::from_secs(60),
        max_outputs: 1,
        credentials: false,
        cleanup: false,
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
        let error = stage
            .upload(job, folder.path(), &[], &[])
            .await
            .unwrap_err();
        assert!(matches!(error, ExecutorError::Unsupported(_)), "{error}");
    }
}

/// A staging root in the S3 bucket of the environment, below its own prefix.
fn s3_stage(name: &str, credentials: bool) -> RemoteStage {
    let options = content_store::S3Options {
        bucket: std::env::var("UWE_S3_BUCKET").expect("UWE_S3_BUCKET is set"),
        prefix: Some(format!("uwe-{name}-{}", std::process::id())),
        endpoint: std::env::var("UWE_S3_ENDPOINT").ok(),
        region: Some(std::env::var("AWS_REGION").unwrap_or_else(|_| "us-east-1".into())),
    };
    RemoteStage {
        objects: Store::s3(&options).unwrap(),
        expiry: Duration::from_secs(600),
        max_outputs: 8,
        credentials,
        cleanup: false,
    }
}

/// A staging root below the address in the environment variable `root`, through the storage
/// providers of `UWE_TEST_STORAGE`, as uwe opens it.
fn configured_stage(root: &str) -> RemoteStage {
    let root = std::env::var(root).expect("the address of the staging root");
    let root = format!("{root}/uwe-remote-{}", std::process::id());
    let text = std::env::var("UWE_TEST_STORAGE").expect("UWE_TEST_STORAGE is set");
    let config = config_file::ConfigFile::parse(&text, Path::new("storage.toml")).unwrap();
    let storage = storage::Storage::from_config(&config).unwrap();
    let storage::StoreLocation::Objects(operator) = storage.store(&root).unwrap() else {
        panic!("{root} is not in an object store");
    };
    RemoteStage {
        objects: Store::remote(operator, &root).unwrap(),
        expiry: Duration::from_secs(600),
        max_outputs: 8,
        credentials: false,
        cleanup: false,
    }
}

/// Writes an object and reads it back through presigned requests alone, as a node does.
async fn presigned_object(stage: &RemoteStage) {
    let client = reqwest::Client::new();
    let send = async |request: Request, body: &'static str| {
        let method = reqwest::Method::from_bytes(request.method.as_bytes()).unwrap();
        let mut built = client.request(method, &request.url).body(body);
        for (name, value) in &request.headers {
            built = built.header(name, value);
        }
        let response = built.send().await.unwrap();
        assert!(response.status().is_success(), "{response:?}");
        response.text().await.unwrap()
    };
    let write = stage
        .request("attempts/presigned/files/1", true)
        .await
        .unwrap();
    send(write, "presigned\n").await;
    let read = stage
        .request("attempts/presigned/files/1", false)
        .await
        .unwrap();
    assert_eq!(send(read, "").await, "presigned\n");
}

/// The emulator answers only requests that it can read without a token from Google, so this
/// checks the presigned requests alone.
#[tokio::test]
#[ignore = "needs fake-gcs-server; run `just emulator-test`"]
async fn gcs_presigned_requests() {
    presigned_object(&configured_stage("UWE_GCS_ROOT")).await;
}

#[tokio::test]
#[ignore = "needs Azurite; run `just emulator-test`"]
async fn azure_round_trip() {
    round_trip(&configured_stage("UWE_AZURE_ROOT")).await;
}

#[tokio::test]
#[ignore = "needs an S3 bucket; set UWE_S3_BUCKET and AWS credentials, then run with --ignored"]
async fn presigned_round_trip() {
    round_trip(&s3_stage("remote", false)).await;
}

#[tokio::test]
#[ignore = "needs an S3 bucket and its provider in UWE_STAGING_STORAGE; run `just s3-test`"]
async fn credentials_round_trip() {
    round_trip(&s3_stage("credentials", true)).await;
}

/// Nodes with credentials download the binary through a presigned URL too, as they have no
/// helper yet.
#[tokio::test]
#[ignore = "needs an S3 bucket and its provider in UWE_STAGING_STORAGE; run `just s3-test`"]
async fn presigns_binary_download() {
    let stage = s3_stage("binary", true);
    let sources = tempfile::tempdir().unwrap();
    let binary = binary(sources.path(), b"static binary\n");
    let url = stage.binary(&binary).await.unwrap();
    assert!(url.starts_with("http"), "{url}");
    let body = reqwest::get(&url)
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    assert_eq!(body.text().await.unwrap(), "static binary\n");
}

/// Sends an output larger than one part as an object in parts, which S3 marks with the count of
/// its parts in the ETag.
#[tokio::test]
#[ignore = "needs an S3 bucket and its provider in UWE_STAGING_STORAGE; run `just s3-test`"]
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
    (tokio::task::spawn_blocking(move || crate::helper::stage_out(&path, None, &plan)).await)
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

#[tokio::test]
async fn collects_patiently() {
    let tries = std::sync::atomic::AtomicU32::new(0);
    let collected = patiently(|| async {
        match tries.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
            0 | 1 => Err(failed("the staging root is down")),
            _ => Ok(7),
        }
    })
    .await;
    assert_eq!(collected.unwrap(), 7);
    let tries = std::sync::atomic::AtomicU32::new(0);
    let cancelled = patiently(|| async {
        tries.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err::<(), _>(ExecutorError::Cancelled)
    })
    .await;
    assert!(matches!(cancelled, Err(ExecutorError::Cancelled)));
    assert_eq!(tries.into_inner(), 1, "a cancel ends at once");
}

#[tokio::test]
async fn remembers_file_digests() {
    let folder = tempfile::tempdir().unwrap();
    let file = folder.path().join("image.tar");
    std::fs::write(&file, "one").unwrap();
    let first = remembered(&file).await.unwrap();
    assert_eq!(first, (Digest::sha256(b"one"), 3));
    // The same size and time of change: the file is not read again.
    let time = std::fs::metadata(&file).unwrap().modified().unwrap();
    std::fs::write(&file, "two").unwrap();
    let set = |time| {
        std::fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(time)
    };
    set(time).unwrap();
    assert_eq!(remembered(&file).await.unwrap(), first);
    set(time + Duration::from_secs(1)).unwrap();
    assert_eq!(
        remembered(&file).await.unwrap(),
        (Digest::sha256(b"two"), 3)
    );
}
