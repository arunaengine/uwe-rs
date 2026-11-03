//! Checks refused TES tasks, immutable input reuse and lost submission recovery.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::*;

#[tokio::test]
async fn delivers_native_helper() {
    let folder = tempfile::tempdir().unwrap();
    let (_, objects, source) = stores();
    let file = folder.path().join("uwe");
    let bytes = b"configured static helper";
    std::fs::write(&file, bytes).unwrap();
    let helper = executor::binary::Binary {
        file,
        digest: Digest::sha256(bytes),
        machine: executor::binary::Machine::Amd64,
        uploaded: Default::default(),
    };
    let stage = ObjectStage::new(
        source,
        objects.clone(),
        Settings {
            helper_file: Some(helper.clone()),
            ..Settings::default()
        },
    )
    .unwrap();
    let mut spec = spec(false);
    spec.output_manifest = Some("cwl.output.json".into());
    let task = stage
        .prepare(
            Job {
                name: "manifest",
                spec: &spec,
                staging: &Staging::default(),
                placement: &Placement::default(),
            },
            false,
        )
        .await
        .unwrap();
    assert_eq!(task.executors[0].command, ["sh", crate::backend::WRAPPER]);
    let input = task
        .inputs
        .iter()
        .find(|input| input.path == crate::backend::HELPER)
        .unwrap();
    assert!(
        input
            .url
            .ends_with(&content_store::layout::content(&helper.digest))
    );
    assert_eq!(
        objects.get_content(&helper.digest).await.unwrap().as_ref(),
        bytes
    );
    let input = task
        .inputs
        .iter()
        .find(|input| input.path == crate::backend::WRAPPER)
        .unwrap();
    let location = input.url.strip_prefix("s3://bucket/tes/").unwrap();
    let script = String::from_utf8(objects.get_object(location).await.unwrap().to_vec()).unwrap();
    assert!(script.contains("'x86_64'"));
    assert!(script.contains(helper.digest.hex()));
    assert!(script.contains("'/uwe/helper' __stage discover /uwe"));
    assert!(
        script.find(": > \"$control/objects\"").unwrap() < script.find("__stage discover").unwrap()
    );
}

#[tokio::test]
#[ignore = "bounded local staging benchmark; run with --ignored --nocapture"]
async fn benchmarks_reference_staging() {
    let folder = tempfile::tempdir().unwrap();
    let (_, objects, source) = stores();
    let stage = ObjectStage::new(source.clone(), objects, Settings::default()).unwrap();
    let mut spec = spec(false);
    let mut staging = Staging::default();
    for index in 0..512 {
        let name = format!("reference-{index:03}");
        let mut bytes = vec![b'a'; 4096];
        bytes[..8].copy_from_slice(&(index as u64).to_le_bytes());
        let path = folder.path().join(&name);
        std::fs::write(&path, &bytes).unwrap();
        spec.inputs.push(InputFile {
            path: name.clone(),
            digest: Digest::sha256(&bytes),
            kind: FileKind::File,
            executable: false,
            writable: false,
        });
        staging.inputs.push(Staged {
            path: name,
            source: path,
            ..Staged::default()
        });
    }
    for round in ["cold", "warm"] {
        let start = std::time::Instant::now();
        let task = stage
            .prepare(
                Job {
                    name: "benchmark-1",
                    spec: &spec,
                    staging: &staging,
                    placement: &Placement::default(),
                },
                false,
            )
            .await
            .unwrap();
        assert_eq!(task.inputs.len(), 512);
        eprintln!(
            "TES reference staging: round={round} files=512 bytes=2097152 elapsed_us={}",
            start.elapsed().as_micros()
        );
    }
}

#[tokio::test]
async fn stages_indexed_tree() {
    let folder = tempfile::tempdir().unwrap();
    let (operator, objects, source) = stores();
    let mut entries = Vec::new();
    for index in 0..32 {
        let path = format!("file-{index:02}");
        let bytes = path.as_bytes();
        std::fs::write(folder.path().join(&path), bytes).unwrap();
        entries.push(TreeEntry::File {
            path: path.clone(),
            digest: Digest::sha256(bytes),
            size: bytes.len() as u64,
            executable: false,
        });
    }
    let digest = source
        .put_tree(&task_model::Tree { entries })
        .await
        .unwrap();
    let mut spec = spec(false);
    spec.inputs.push(InputFile {
        path: "reference".into(),
        kind: FileKind::Directory,
        digest,
        executable: false,
        writable: false,
    });
    let staging = Staging {
        inputs: vec![Staged {
            path: "reference".into(),
            source: folder.path().to_owned(),
            directory: true,
            ..Staged::default()
        }],
        ..Staging::default()
    };
    let stage = ObjectStage::new(source, objects.clone(), Settings::default()).unwrap();
    let task = stage
        .prepare(
            Job {
                name: "reference-1",
                spec: &spec,
                staging: &staging,
                placement: &Placement::default(),
            },
            false,
        )
        .await
        .unwrap();
    assert_eq!(task.inputs.len(), 32);
    for input in task.inputs {
        let name = input.path.rsplit('/').next().unwrap();
        let key = objects.location_of(&input.url).unwrap();
        assert_eq!(
            operator.read(&key).await.unwrap().to_bytes().as_ref(),
            name.as_bytes()
        );
    }
}

#[tokio::test]
async fn refuses_unrepresentable_tasks() {
    let folder = tempfile::tempdir().unwrap();
    let (_, objects, source) = stores();
    let stage = ObjectStage::new(source, objects, Settings::default()).unwrap();
    let input = folder.path().join("input");
    std::fs::write(&input, b"data").unwrap();
    let staged = Staged {
        path: "in.txt".into(),
        source: input.clone(),
        ..Staged::default()
    };
    let mut cases = Vec::new();
    let mut stdin = spec(false);
    stdin.stdin = Some(Digest::sha256(b"input"));
    cases.push((stdin, Staging::default()));
    let mut directory = spec(false);
    directory.outputs[0].pattern = "out/".into();
    cases.push((directory, Staging::default()));
    let mut overlap = spec(false);
    overlap.outputs[0].pattern = "in.txt".into();
    cases.push((
        overlap,
        Staging {
            inputs: vec![staged.clone()],
            ..Staging::default()
        },
    ));
    cases.push((
        spec(false),
        Staging {
            inputs: vec![Staged {
                path: "in*".into(),
                ..staged.clone()
            }],
            ..Staging::default()
        },
    ));
    cases.push((
        spec(false),
        Staging {
            inputs: vec![Staged {
                in_place: true,
                ..staged.clone()
            }],
            ..Staging::default()
        },
    ));
    cases.push((
        spec(false),
        Staging {
            inputs: vec![Staged {
                executable: true,
                ..staged.clone()
            }],
            ..Staging::default()
        },
    ));
    cases.push((
        spec(false),
        Staging {
            directories: vec!["empty".into()],
            ..Staging::default()
        },
    ));
    let link = folder.path().join("link");
    std::os::unix::fs::symlink(&input, &link).unwrap();
    cases.push((
        spec(false),
        Staging {
            inputs: vec![Staged {
                source: link,
                ..staged
            }],
            ..Staging::default()
        },
    ));
    for (spec, staging) in cases {
        let error = stage
            .prepare(
                Job {
                    name: "refused-1",
                    spec: &spec,
                    staging: &staging,
                    placement: &Placement::default(),
                },
                false,
            )
            .await
            .unwrap_err();
        assert!(matches!(error, ExecutorError::Unsupported(_)), "{error}");
        assert!(!error.retryable());
    }
}

#[tokio::test]
async fn reuses_bucket_content() {
    let folder = tempfile::tempdir().unwrap();
    let (operator, objects, _) = stores();
    let source = Store::remote(operator, "s3://bucket/cache").unwrap();
    let digest = source.put_content(b"data".to_vec().into()).await.unwrap();
    let input = folder.path().join("input");
    std::fs::write(&input, b"data").unwrap();
    let stage = ObjectStage::new(source.clone(), objects, Settings::default()).unwrap();
    let mut spec = spec(false);
    spec.inputs.push(InputFile {
        path: "in.txt".into(),
        kind: FileKind::File,
        digest: digest.clone(),
        executable: false,
        writable: false,
    });
    let staging = Staging {
        inputs: vec![Staged {
            path: "in.txt".into(),
            source: input,
            ..Staged::default()
        }],
        ..Staging::default()
    };
    let task = stage
        .prepare(
            Job {
                name: "reuse-1",
                spec: &spec,
                staging: &staging,
                placement: &Placement::default(),
            },
            false,
        )
        .await
        .unwrap();
    assert_eq!(
        task.inputs[0].url,
        source.url(&content_store::layout::content(&digest))
    );
}

#[tokio::test]
async fn recovers_lost_response() {
    let folder = tempfile::tempdir().unwrap();
    let (operator, objects, source) = stores();
    let server = server(operator, objects.clone(), "RUNNING").await;
    let backend = backend(&server, folder.path(), &source, &objects);
    let spec = spec(false);
    let staging = Staging::default();
    let placement = Placement::default();
    let job = Job {
        name: "crashed-1",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let mut task = backend
        .staging
        .as_ref()
        .unwrap()
        .prepare(job, false)
        .await
        .unwrap();
    task.tags.insert(
        backend.settings.idempotency_tag.clone().unwrap(),
        executor::capture::attempt_prefix(job).unwrap(),
    );
    backend.client.create(&task).await.unwrap();
    let root = backend.scratch.join(job.name);
    std::fs::create_dir_all(&root).unwrap();
    let journal = json!({"backend": "TES", "server": backend.client.server(), "name": job.name,
        "id": null, "owner": u32::MAX, "started": "", "host": std::fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap(),
        "request": task, "idempotency_tag": backend.settings.idempotency_tag, "auth": backend.client.identity()});
    std::fs::write(root.join("remote.json"), journal.to_string()).unwrap();
    let original = backend.client.clone();
    let mut backend = backend;
    backend.client = Client::new(&server.address, Some("another-caller".into())).unwrap();
    // Other credentials neither submit again nor drop the journal, and block nothing.
    backend.reconcile(&[&backend]).await.unwrap();
    assert_eq!(server.documents.lock().unwrap().len(), 1);
    assert!(root.join("remote.json").exists());
    backend.client = original;
    backend.reconcile(&[&backend]).await.unwrap();
    assert_eq!(server.documents.lock().unwrap().len(), 1);
    assert!(!root.join("remote.json").exists());
}

#[tokio::test]
async fn bounds_expanded_inputs() {
    let folder = tempfile::tempdir().unwrap();
    let (_, objects, source) = stores();
    let stage = ObjectStage::new(
        source,
        objects,
        Settings {
            max_inputs: 1,
            ..Settings::default()
        },
    )
    .unwrap();
    let input = folder.path().join("input");
    std::fs::write(&input, b"data").unwrap();
    let staging = Staging {
        inputs: vec![Staged {
            path: "in.txt".into(),
            source: input,
            ..Staged::default()
        }],
        ..Staging::default()
    };
    let error = stage
        .prepare(
            Job {
                name: "limit-1",
                spec: &spec(true),
                staging: &staging,
                placement: &Placement::default(),
            },
            false,
        )
        .await
        .unwrap_err();
    assert!(matches!(error, ExecutorError::Unsupported(_)));
}

#[tokio::test]
async fn omits_unconfigured_idempotency() {
    let folder = tempfile::tempdir().unwrap();
    let (operator, objects, source) = stores();
    let server = server(operator, objects.clone(), "COMPLETE").await;
    let mut backend = backend(&server, folder.path(), &source, &objects);
    backend.settings.idempotency_tag = None;
    backend
        .run(
            Job {
                name: "untagged-1",
                spec: &spec(false),
                staging: &Staging::default(),
                placement: &Placement::default(),
            },
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    let tags = &server.documents.lock().unwrap()[0]["tags"];
    let keys: Vec<&String> = tags.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["uwe-attempt", "uwe-submission"]);
}

#[tokio::test]
async fn adds_static_tags() {
    let folder = tempfile::tempdir().unwrap();
    let (operator, objects, source) = stores();
    let server = server(operator, objects.clone(), "COMPLETE").await;
    let mut backend = backend(&server, folder.path(), &source, &objects);
    backend.settings.tags = BTreeMap::from([("example.org/network".into(), "open".into())]);
    backend
        .run(
            Job {
                name: "tagged-1",
                spec: &spec(false),
                staging: &Staging::default(),
                placement: &Placement::default(),
            },
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    let tags = &server.documents.lock().unwrap()[0]["tags"];
    assert_eq!(tags["example.org/network"], "open");
    assert_eq!(tags["uwe-attempt"], "tagged-1");
}

#[tokio::test]
async fn preserves_encoded_paths() {
    let folder = tempfile::tempdir().unwrap();
    let (operator, objects, source) = stores();
    let stage = ObjectStage::new(source.clone(), objects, Settings::default()).unwrap();
    let mut spec = spec(false);
    spec.outputs[0].pattern = "out/a %+#?.txt".into();
    let staging = Staging::default();
    let placement = Placement::default();
    let job = Job {
        name: "encoded-1",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let task = stage.prepare(job, false).await.unwrap();
    let path = "/uwe/work/out/a %+#?.txt";
    let prefix = executor::capture::attempt_prefix(job).unwrap();
    operator
        .write(&format!("{prefix}/root{path}"), b"data".to_vec())
        .await
        .unwrap();
    let url = format!("{}a%20%25%2B%23%3F.txt", task.outputs[0].url);
    let status = serde_json::from_value(json!({ "id": "encoded", "state": "COMPLETE", "logs": [{ "outputs": [{ "path": path, "url": url, "size_bytes": "4" }] }] })).unwrap();
    let finished = stage
        .collect(job, &task, &status, folder.path())
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(finished.local_path(&spec.outputs[0].pattern)).unwrap(),
        b"data"
    );
}
