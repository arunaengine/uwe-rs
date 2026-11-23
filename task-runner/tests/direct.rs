//! Checks remote inputs that stay where they lie, remote directories and output receipts.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

// The futures of attempts nest deeply, deeper than the default limit lets the compiler check.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use content_store::{Store, layout};
use executor::manifest::Collected;
use executor::{Backend, ExecutorError, Finished, Job, OutputReceipt, Staging};
use futures_util::future::BoxFuture;
use storage::{Details, DirectRef, Entry, Metadata, Storage, StorageError, StorageProvider};
use task_model::{Digest, FileKind};
use task_runner::{Runner, RunnerError, Settings};
use tokio_util::sync::CancellationToken;
use workflow_graph::{Input, Task, TaskId};

/// Objects of `aruna://` addresses in memory, with or without their digests, and a count of
/// the downloads of each.
struct Aruna {
    objects: BTreeMap<String, Vec<u8>>,
    digests: bool,
    downloads: Mutex<usize>,
}

impl Aruna {
    fn new(objects: &[(&str, &str)], digests: bool) -> Arc<Aruna> {
        let objects = (objects.iter())
            .map(|(address, text)| ((*address).to_owned(), text.as_bytes().to_vec()))
            .collect();
        Arc::new(Aruna {
            objects,
            digests,
            downloads: Mutex::new(0),
        })
    }

    fn downloads(&self) -> usize {
        *self.downloads.lock().unwrap()
    }
}

impl StorageProvider for Aruna {
    fn name(&self) -> &str {
        "aruna"
    }

    fn matches(&self, address: &str) -> bool {
        address.starts_with("aruna://")
    }

    fn stat<'a>(
        &'a self,
        address: &'a str,
    ) -> BoxFuture<'a, Result<Option<Details>, StorageError>> {
        Box::pin(async move {
            let metadata = |directory, size| Metadata {
                directory,
                modified: None,
                size,
                version: None,
            };
            if let Some(bytes) = self.objects.get(address) {
                let digest = Digest::sha256(bytes).hex().to_owned();
                return Ok(Some(Details {
                    metadata: Metadata {
                        version: Some(digest.clone()),
                        ..metadata(false, bytes.len() as u64)
                    },
                    digest: self.digests.then_some(digest),
                    endpoints: vec!["https://proxy.example.org".into()],
                    direct: self.digests.then(|| DirectRef {
                        url: format!("https://proxy.example.org/{}", &address[8..]),
                        aruna_id: Some(format!("object-{}", address.len())),
                        // A signed object needs a header that some workers cannot send.
                        headers: (address.contains("signed"))
                            .then(|| ("authorization".to_owned(), "Bearer x".to_owned()))
                            .into_iter()
                            .collect(),
                        ..DirectRef::default()
                    }),
                }));
            }
            let folder = format!("{}/", address.trim_end_matches('/'));
            let found = self.objects.keys().any(|key| key.starts_with(&folder));
            Ok(found.then(|| Details::of(metadata(true, 0))))
        })
    }

    fn list<'a>(&'a self, address: &'a str) -> BoxFuture<'a, Result<Vec<Entry>, StorageError>> {
        Box::pin(async move {
            let folder = format!("{}/", address.trim_end_matches('/'));
            let mut entries: Vec<Entry> = Vec::new();
            for rest in self
                .objects
                .keys()
                .filter_map(|key| key.strip_prefix(&folder))
            {
                let (name, directory) = match rest.split_once('/') {
                    Some((name, _)) => (name, true),
                    None => (rest, false),
                };
                if !entries.iter().any(|entry| entry.name == name) {
                    let name = name.to_owned();
                    entries.push(Entry { name, directory });
                }
            }
            Ok(entries)
        })
    }

    fn download<'a>(
        &'a self,
        address: &'a str,
        target: &'a Path,
    ) -> BoxFuture<'a, Result<Option<String>, StorageError>> {
        Box::pin(async move {
            *self.downloads.lock().unwrap() += 1;
            let bytes = &self.objects[address];
            std::fs::write(target, bytes).unwrap();
            Ok(Some(Digest::sha256(bytes).hex().to_owned()))
        })
    }
}

/// A backend that records what it staged and writes `out.txt`, or, with a store, puts
/// `promoted` into the store itself and gives a receipt for it.
struct Remote {
    scratch: PathBuf,
    direct: bool,
    staged: Mutex<Vec<Staging>>,
    promote: Option<(Store, &'static str, &'static str)>,
    /// The files of the staged directories, by their paths in them, while the task runs.
    files: Mutex<BTreeMap<String, String>>,
}

impl Backend for Remote {
    async fn pin(&self, image: &str) -> Result<Option<String>, ExecutorError> {
        Ok(Some(image.to_owned()))
    }

    async fn run(
        &self,
        job: Job<'_>,
        _cancel: &CancellationToken,
    ) -> Result<Finished, ExecutorError> {
        self.staged.lock().unwrap().push(job.staging.clone());
        for input in job.staging.inputs.iter().filter(|input| input.directory) {
            for name in ["genome.fa", "parts/a.bin"] {
                let text = std::fs::read_to_string(input.source.join(name))?;
                self.files.lock().unwrap().insert(name.to_owned(), text);
            }
        }
        let root = self.scratch.join(job.name);
        std::fs::create_dir_all(root.join("work"))?;
        std::fs::create_dir_all(root.join("control"))?;
        let mut finished = Finished {
            address: None,
            exit_code: Some(0),
            files: vec![Collected {
                path: "out.txt".into(),
                directory: false,
            }],
            root,
            usage: None,
            receipts: Vec::new(),
        };
        match &self.promote {
            Some((store, text, location)) => {
                let digest = Digest::sha256(text.as_bytes());
                let held = if text.is_empty() {
                    digest.clone()
                } else {
                    store
                        .put_content(text.as_bytes().to_vec().into())
                        .await
                        .unwrap()
                };
                finished.receipts.push(OutputReceipt {
                    path: "out.txt".into(),
                    digest: held,
                    size: 9,
                    location: (*location).into(),
                });
            }
            None => std::fs::write(finished.local_path("out.txt"), "made\n")?,
        }
        Ok(finished)
    }

    fn direct_sources(&self) -> bool {
        self.direct
    }

    /// The workers send no headers, as those of TES do not.
    fn supports_direct(&self, reference: &DirectRef) -> bool {
        self.direct && reference.headers.is_empty()
    }
}

fn runner(root: &Path, run: &str, direct: bool, storage: &Storage) -> Runner<Remote> {
    let store = Store::local(&root.join("store")).unwrap();
    let backend = Remote {
        scratch: root.join("scratch"),
        direct,
        staged: Mutex::new(Vec::new()),
        promote: None,
        files: Mutex::default(),
    };
    let settings = Settings {
        downloads: root.join("downloads"),
        import_inputs: false,
        storage: storage.session(),
        digests: None,
    };
    Runner::new(store, backend, run, settings)
}

fn task(source: &str, directory: bool) -> Task {
    Task {
        id: TaskId(1),
        name: "align (1)".into(),
        step: "align".into(),
        network: workflow_graph::Network::Denied,
        image: Some("debian@sha256:00".into()),
        command: vec!["sh".into(), "-c".into(), "cat reads > out.txt".into()],
        inputs: vec![Input {
            path: "reads".into(),
            source: source.into(),
            directory,
            ..Default::default()
        }],
        outputs: vec!["out.txt".into()],
        ..Task::default()
    }
}

/// Runs the task once with the runner and says whether the cache answered it.
async fn run(runner: &Runner<Remote>, task: &Task) -> Result<bool, RunnerError> {
    let prepared = runner.request(task, task.image.clone()).await?;
    let result = runner.execute(&prepared, &CancellationToken::new()).await?;
    Ok(result.reused)
}

#[tokio::test]
async fn passes_remote_inputs() {
    let root = tempfile::tempdir().unwrap();
    let aruna = Aruna::new(&[("aruna://project/reads.fq", "ACGT\n")], true);
    let storage = Storage::default().with_provider(aruna.clone());
    let source = "aruna://project/reads.fq";
    let first = runner(root.path(), "first", true, &storage);
    assert!(!run(&first, &task(source, false)).await.unwrap());
    let staged = first.backend().staged.lock().unwrap().clone();
    assert_eq!(staged.len(), 1);
    assert!(
        staged[0].inputs.is_empty(),
        "nothing goes through this machine"
    );
    let direct = &staged[0].direct;
    assert_eq!(direct.len(), 1);
    assert_eq!(direct[0].path, "reads");
    assert_eq!(direct[0].source, source);
    assert_eq!(direct[0].endpoints, ["https://proxy.example.org"]);
    assert_eq!(direct[0].digest, Digest::sha256(b"ACGT\n"));
    assert_eq!(direct[0].size, 5);
    assert_eq!(
        direct[0].reference.url,
        "https://proxy.example.org/project/reads.fq"
    );
    assert_eq!(aruna.downloads(), 0, "a cache miss downloads nothing here");
    // Another run finds the result by the same digest, again without the bytes.
    let second = runner(root.path(), "second", true, &storage);
    assert!(run(&second, &task(source, false)).await.unwrap());
    assert!(second.backend().staged.lock().unwrap().is_empty());
    assert_eq!(aruna.downloads(), 0, "a cache hit transfers no byte");
}

/// An input that the workers read where it lies is in the record of what the run read, with
/// the digest, size and version of its provider.
#[tokio::test]
async fn records_direct_reads() {
    let root = tempfile::tempdir().unwrap();
    let aruna = Aruna::new(&[("aruna://project/reads.fq", "ACGT\n")], true);
    let storage = Storage::default().with_provider(aruna.clone());
    let backend = Remote {
        scratch: root.path().join("scratch"),
        direct: true,
        staged: Mutex::new(Vec::new()),
        promote: None,
        files: Mutex::default(),
    };
    let settings = Settings {
        downloads: root.path().join("downloads"),
        import_inputs: false,
        storage: storage.clone(),
        digests: None,
    };
    let store = Store::local(&root.path().join("store")).unwrap();
    let runner = Runner::new(store, backend, "recorded", settings);
    assert!(
        !run(&runner, &task("aruna://project/reads.fq", false))
            .await
            .unwrap()
    );
    assert_eq!(aruna.downloads(), 0);
    let fetched = storage.fetched();
    let digest = Digest::sha256(b"ACGT\n");
    assert_eq!(fetched.len(), 1);
    assert_eq!(fetched[0].address, "aruna://project/reads.fq");
    assert_eq!(fetched[0].digest, digest.to_string());
    assert_eq!(fetched[0].size, 5);
    assert_eq!(fetched[0].version.as_deref(), Some(digest.hex()));
    assert!(fetched[0].copy.as_os_str().is_empty(), "no copy here");
}

#[tokio::test]
async fn downloads_without_direct() {
    let root = tempfile::tempdir().unwrap();
    let aruna = Aruna::new(&[("aruna://project/reads.fq", "ACGT\n")], true);
    let storage = Storage::default().with_provider(aruna.clone());
    let source = "aruna://project/reads.fq";
    let local = runner(root.path(), "local", false, &storage);
    assert!(!run(&local, &task(source, false)).await.unwrap());
    let staged = local.backend().staged.lock().unwrap().clone();
    assert!(staged[0].direct.is_empty());
    assert_eq!(staged[0].inputs.len(), 1);
    assert_eq!(
        std::fs::read(&staged[0].inputs[0].source).unwrap(),
        b"ACGT\n"
    );
    assert_eq!(aruna.downloads(), 1);
    // The verified digest gives the same cache key as the downloaded bytes.
    let remote = runner(root.path(), "remote", true, &storage);
    assert!(run(&remote, &task(source, false)).await.unwrap());
    assert_eq!(aruna.downloads(), 1);
}

/// An input whose reference needs headers that the workers cannot send is staged as a verified
/// local file, while the other inputs of the backend stay where they lie.
#[tokio::test]
async fn stages_header_inputs() {
    let root = tempfile::tempdir().unwrap();
    let aruna = Aruna::new(&[("aruna://project/signed.fq", "ACGT\n")], true);
    let storage = Storage::default().with_provider(aruna.clone());
    let source = "aruna://project/signed.fq";
    let runner = runner(root.path(), "signed", true, &storage);
    assert!(!run(&runner, &task(source, false)).await.unwrap());
    let staged = runner.backend().staged.lock().unwrap().clone();
    assert!(staged[0].direct.is_empty());
    assert_eq!(staged[0].inputs.len(), 1);
    let bytes = std::fs::read(&staged[0].inputs[0].source).unwrap();
    assert_eq!(bytes, b"ACGT\n");
    assert_eq!(aruna.downloads(), 1);
}

#[tokio::test]
async fn passes_remote_trees() {
    let root = tempfile::tempdir().unwrap();
    let objects = [
        ("aruna://project/index/genome.fa", ">chr1\nACGT\n"),
        ("aruna://project/index/parts/a.bin", "a"),
        ("aruna://project/other.txt", "other"),
    ];
    let aruna = Aruna::new(&objects, true);
    let storage = Storage::default().with_provider(aruna.clone());
    let source = "aruna://project/index";
    let direct = runner(root.path(), "direct", true, &storage);
    assert!(!run(&direct, &task(source, true)).await.unwrap());
    let staged = direct.backend().staged.lock().unwrap().clone();
    let paths: Vec<&str> = staged[0]
        .direct
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    assert_eq!(paths, ["reads/genome.fa", "reads/parts/a.bin"]);
    assert_eq!(
        staged[0].direct[1].source,
        "aruna://project/index/parts/a.bin"
    );
    assert_eq!(staged[0].directories, ["reads"]);
    assert_eq!(aruna.downloads(), 0);
    // A provider without digests gives a private copy of the directory, with the same key.
    let plain = Aruna::new(&objects, false);
    let storage = Storage::default().with_provider(plain.clone());
    let copied = runner(root.path(), "copied", false, &storage);
    let tree = task(source, true);
    let prepared = copied.request(&tree, tree.image.clone()).await.unwrap();
    assert_eq!(prepared.request.spec.inputs[0].kind, FileKind::Directory);
    assert_eq!(plain.downloads(), 2);
    let result = copied.execute(&prepared, &CancellationToken::new()).await;
    assert!(
        result.unwrap().reused,
        "both ways give the directory one digest"
    );
    let fresh = tempfile::tempdir().unwrap();
    let snapshot = runner(fresh.path(), "snapshot", false, &storage);
    assert!(!run(&snapshot, &task(source, true)).await.unwrap());
    let staged = snapshot.backend().staged.lock().unwrap().clone();
    assert!(staged[0].inputs[0].directory);
    let files = snapshot.backend().files.lock().unwrap().clone();
    assert_eq!(files["genome.fa"], ">chr1\nACGT\n");
    assert_eq!(files["parts/a.bin"], "a");
}

#[tokio::test]
async fn records_output_receipts() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::local(&root.path().join("store")).unwrap();
    let settings = Settings {
        downloads: root.path().join("downloads"),
        import_inputs: false,
        storage: Storage::default(),
        digests: None,
    };
    let local = root.path().join("input.txt");
    std::fs::write(&local, "data\n").unwrap();
    let source = local.to_string_lossy();
    // A receipt that names another place in the store still records the content of its digest.
    for (text, run, location) in [
        ("promoted\n", "kept", "aruna://project/out.txt"),
        ("promoted\n", "elsewhere", "../../outside/out.txt"),
        ("", "unconfirmed", "aruna://project/out.txt"),
    ] {
        let backend = Remote {
            scratch: root.path().join("scratch"),
            direct: true,
            staged: Mutex::new(Vec::new()),
            promote: Some((store.clone(), text, location)),
            files: Mutex::default(),
        };
        let runner = Runner::new(store.clone(), backend, run, settings.clone());
        let mut task = task(&source, false);
        task.command[2] = format!("echo {run}");
        let prepared = runner.request(&task, task.image.clone()).await.unwrap();
        let result = runner.execute(&prepared, &CancellationToken::new()).await;
        if text.is_empty() {
            let error = result.expect_err("the store holds no such content");
            assert!(matches!(error, RunnerError::Receipt(path) if path == "out.txt"));
            continue;
        }
        let result = result.unwrap();
        let digest = Digest::sha256(text.as_bytes());
        assert_eq!(result.files.len(), 1);
        assert_eq!(result.files[0].digest, digest);
        assert_eq!(result.files[0].size, 9);
        assert_eq!(result.files[0].location, layout::content(&digest));
        let read = runner.read(&result.files[0].location).await.unwrap();
        assert_eq!(&read[..], text.as_bytes());
    }
}

#[tokio::test]
async fn separates_tenant_caches() {
    let root = tempfile::tempdir().unwrap();
    let aruna = Aruna::new(&[("aruna://project/reads.fq", "ACGT\n")], true);
    let storage = Storage::default().with_provider(aruna.clone());
    let task = task("aruna://project/reads.fq", false);
    let tenant = |run: &str, namespace: Option<&str>| {
        let mut runner = runner(root.path(), run, true, &storage);
        runner.cache_namespace(namespace);
        runner
    };
    assert!(!run(&tenant("a1", Some("project:a")), &task).await.unwrap());
    assert!(run(&tenant("a2", Some("project:a")), &task).await.unwrap());
    assert!(!run(&tenant("b1", Some("project:b")), &task).await.unwrap());
    // The shared namespace holds neither result.
    let shared = tenant("shared", None);
    assert!(!run(&shared, &task).await.unwrap());
    let prepared = shared.request(&task, task.image.clone()).await.unwrap();
    assert_eq!(prepared.request.spec.namespace, "network-denied");
}
