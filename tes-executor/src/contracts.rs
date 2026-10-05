//! Checks refused TES tasks, immutable input reuse and lost submission recovery.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::*;

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
            .prepare(Job {
                name: "refused-1",
                spec: &spec,
                staging: &staging,
                placement: &Placement::default(),
            })
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
        .prepare(Job {
            name: "reuse-1",
            spec: &spec,
            staging: &staging,
            placement: &Placement::default(),
        })
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
        .prepare(job)
        .await
        .unwrap();
    task.tags.insert(
        backend.settings.idempotency_tag.clone().unwrap(),
        attempt_prefix(job).unwrap(),
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
    assert!(backend.reconcile(&[&backend]).await.is_err());
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
        .prepare(Job {
            name: "limit-1",
            spec: &spec(true),
            staging: &staging,
            placement: &Placement::default(),
        })
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
    assert_eq!(
        server.documents.lock().unwrap()[0]["tags"]
            .as_object()
            .unwrap()
            .len(),
        1
    );
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
    let task = stage.prepare(job).await.unwrap();
    let path = "/iunctus/work/out/a %+#?.txt";
    let prefix = attempt_prefix(job).unwrap();
    operator
        .write(&format!("{prefix}/root{path}"), b"data".to_vec())
        .await
        .unwrap();
    let url = format!(
        "{}iunctus/work/out/a%20%25%2B%23%3F.txt",
        task.outputs[0].url
    );
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
