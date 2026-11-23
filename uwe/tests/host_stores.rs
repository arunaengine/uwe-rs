//! Runs whose host keeps the runs and journals in its own stores and hears live task output.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use executor::manifest::Collected;
use executor::orphans::{Orphan, Outcome};
use executor::{DirectSource, Live, LogStream, OutputReceipt};
use futures_util::future::BoxFuture;
use run_database::{MemoryStore, RunReader, RunStore};
use serde_json::json;
use storage::{Details, DirectRef, Metadata, StorageError, StorageProvider};
use task_model::Digest;
use uwe::run::{Event, HostExecutor, HostStorage, Settings, TaskState};
use uwe::{RunRequest, setup::Setup};

/// A backend of the host that says a secret on standard error while each task runs, and that
/// reports one job its run left.
struct Talking {
    scratch: std::path::PathBuf,
    stopped: Mutex<Vec<String>>,
}

impl executor::Backend for Talking {
    async fn pin(&self, image: &str) -> Result<Option<String>, executor::ExecutorError> {
        Ok(Some(format!("{image}@sha256:{}", "a".repeat(64))))
    }

    async fn run(
        &self,
        job: executor::Job<'_>,
        _cancel: &executor::CancellationToken,
    ) -> Result<executor::Finished, executor::ExecutorError> {
        if let Some(live) = &job.staging.live {
            live.log(LogStream::Stderr, "token=hunter2\n");
            live.send(Live::Usage {
                cpu_percent: Some(12.5),
                memory_bytes: Some(4096),
            });
        }
        let finished = executor::Finished {
            exit_code: Some(0),
            files: Vec::new(),
            root: self.scratch.join(job.name),
            usage: None,
            address: None,
            receipts: Vec::new(),
        };
        std::fs::create_dir_all(finished.root.join("work"))?;
        std::fs::create_dir_all(finished.root.join("control"))?;
        std::fs::write(finished.stdout(), "3\n")?;
        std::fs::write(finished.root.join("control/exit"), "0\n")?;
        Ok(finished)
    }

    async fn orphans(&self, run: &str) -> executor::OrphansResult {
        Ok(vec![orphan(run, Outcome::Running)])
    }

    async fn stop_orphans(&self, run: &str) -> executor::OrphansResult {
        self.stopped.lock().unwrap().push(run.to_owned());
        Ok(vec![orphan(run, Outcome::Canceled)])
    }
}

fn orphan(run: &str, outcome: Outcome) -> Orphan {
    Orphan {
        attempt: format!("{run}-T-1-1"),
        id: Some("job-1".into()),
        state: None,
        outcome,
    }
}

fn settings(root: &std::path::Path, backend: Arc<Talking>) -> Settings {
    let target = root.join("tool.cwl");
    std::fs::write(&target,
        "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: echo\ninputs:\n  count: int\noutputs: []\n",
    ).unwrap();
    let mut settings = Settings::new(target);
    settings.database = Some(root.join("runs.sqlite"));
    settings.store = Some(root.join("store").to_string_lossy().into_owned());
    settings.cache_folder = Some(root.join("cache"));
    settings.output_dir = Some(root.join("out"));
    settings.setup = Some(Setup::default());
    settings.inputs = Some(json!({"count": 3}));
    settings.default_image = Some("tool:1".into());
    settings.host_executor = Some(HostExecutor::new(backend));
    settings
}

#[tokio::test]
async fn records_into_stores() {
    let root = tempfile::tempdir().unwrap();
    let backend = Arc::new(Talking {
        scratch: root.path().join("host"),
        stopped: Mutex::default(),
    });
    let store = Arc::new(MemoryStore::new().unwrap());
    let (sender, heard) = mpsc::channel();
    let sender = Mutex::new(sender);
    let mut settings = settings(root.path(), backend);
    settings.run_store = Some(store.clone() as Arc<dyn RunStore>);
    settings.secrets.insert("TOKEN", "hunter2").unwrap();
    settings.listener = Some(Arc::new(move |event: Event| {
        let _ = sender.lock().unwrap().send(event);
    }));
    let run = RunRequest::new(settings).run().await.unwrap();
    assert!(run.execution.error.is_none(), "{:?}", run.execution.error);
    let recorded = store.run(&run.id).unwrap().expect("the host keeps the run");
    assert_eq!(
        (recorded.executor.as_str(), recorded.state.as_str()),
        ("host", "succeeded")
    );
    assert_eq!(store.tasks(&run.id).unwrap()[0].state, "succeeded");
    let local = run_database::Database::open(&root.path().join("runs.sqlite")).unwrap();
    assert!(
        local.run(&run.id).unwrap().is_none(),
        "the run is not in the file"
    );
    // The live events come through a queue of their own, after or before the end of the run.
    let (mut log, mut usage) = (None, None);
    while log.is_none() || usage.is_none() {
        match heard
            .recv_timeout(Duration::from_secs(60))
            .expect("live events")
        {
            Event::Log { chunk, stream, .. } => log = Some((stream, chunk)),
            Event::Usage { memory_bytes, .. } => usage = memory_bytes,
            _ => {}
        }
    }
    assert_eq!(log, Some((LogStream::Stderr, "token=[secret]\n".into())));
    assert_eq!(usage, Some(4096));
}

#[tokio::test]
async fn stops_host_orphans() {
    let root = tempfile::tempdir().unwrap();
    let backend = Arc::new(Talking {
        scratch: root.path().join("host"),
        stopped: Mutex::default(),
    });
    let mut settings = settings(root.path(), backend.clone());
    settings.run_id = Some("left-run".into());
    settings.journal_store = Some(Arc::new(executor::MemoryJournal::new()));
    let found = RunRequest::new(settings).remote_tasks().await.unwrap();
    assert_eq!(found, [orphan("left-run", Outcome::Running)]);
    let mut settings = self::settings(root.path(), backend.clone());
    settings.run_id = Some("left-run".into());
    let stopped = RunRequest::new(settings).stop_tasks().await.unwrap();
    assert_eq!(stopped, [orphan("left-run", Outcome::Canceled)]);
    assert_eq!(*backend.stopped.lock().unwrap(), ["left-run"]);
}

/// The object that the bridge test reads, and the replicas that hold it, nearest first.
const READS: &str = "aruna://object/01J9READS";
const REPLICAS: [&str; 2] = ["https://proxy-a.example.org", "https://proxy-b.example.org"];

/// Objects of a host like Aruna in memory, with their verified digests, sizes and replicas,
/// and a count of the downloads through this machine.
struct Objects {
    objects: BTreeMap<String, Vec<u8>>,
    downloads: AtomicUsize,
}

impl StorageProvider for Objects {
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
            let Some(bytes) = self.objects.get(address) else {
                return Ok(None);
            };
            let digest = Digest::sha256(bytes).hex().to_owned();
            let id = address.rsplit('/').next().unwrap_or_default();
            Ok(Some(Details {
                metadata: Metadata {
                    directory: false,
                    modified: None,
                    size: bytes.len() as u64,
                    version: Some(digest.clone()),
                },
                digest: Some(digest),
                endpoints: REPLICAS.map(str::to_owned).to_vec(),
                direct: Some(DirectRef {
                    url: format!("{}/objects/{id}", REPLICAS[0]),
                    aruna_id: Some(id.to_owned()),
                    endpoint: Some(REPLICAS[0].to_owned()),
                    ..DirectRef::default()
                }),
            }))
        })
    }

    fn download<'a>(
        &'a self,
        address: &'a str,
        target: &'a std::path::Path,
    ) -> BoxFuture<'a, Result<Option<String>, StorageError>> {
        Box::pin(async move {
            self.downloads.fetch_add(1, Ordering::SeqCst);
            std::fs::write(target, &self.objects[address]).unwrap();
            Ok(Some(
                Digest::sha256(&self.objects[address]).hex().to_owned(),
            ))
        })
    }
}

/// What the host executor was given for one task.
#[derive(Clone, Debug)]
struct Given {
    sources: Vec<DirectSource>,
    offline: bool,
    cpus: u32,
}

/// A host executor like the one of Aruna: it reads the inputs where they lie, so it decides
/// where each task runs from their replicas, and puts the outputs into the store of the run.
struct Placer {
    store: content_store::Store,
    scratch: std::path::PathBuf,
    given: Mutex<Vec<Given>>,
}

impl executor::Backend for Placer {
    async fn pin(&self, image: &str) -> Result<Option<String>, executor::ExecutorError> {
        Ok(Some(format!("{image}@sha256:{}", "b".repeat(64))))
    }

    async fn run(
        &self,
        job: executor::Job<'_>,
        _cancel: &executor::CancellationToken,
    ) -> Result<executor::Finished, executor::ExecutorError> {
        self.given.lock().unwrap().push(Given {
            sources: job.staging.direct.clone(),
            offline: job.placement.offline,
            cpus: job.placement.cpus,
        });
        if let Some(live) = &job.staging.live {
            live.log(LogStream::Stdout, "placed beside proxy-a\n");
        }
        let counted = b"2 reads\n";
        let digest = (self.store.put_content(counted.to_vec().into()).await)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        let root = self.scratch.join(job.name);
        std::fs::create_dir_all(root.join("work"))?;
        std::fs::create_dir_all(root.join("control"))?;
        std::fs::write(root.join("control/exit"), "0\n")?;
        let finished = executor::Finished {
            exit_code: Some(0),
            files: vec![Collected {
                path: "count.txt".into(),
                directory: false,
            }],
            root,
            usage: None,
            address: None,
            receipts: vec![OutputReceipt {
                path: "count.txt".into(),
                digest,
                size: counted.len() as u64,
                location: "aruna://object/01J9COUNT".into(),
            }],
        };
        std::fs::write(finished.stdout(), "")?;
        Ok(finished)
    }

    fn direct_sources(&self) -> bool {
        true
    }
}

/// A run of a tool that counts the reads of an object of the host, in a cache namespace.
fn bridged(
    root: &std::path::Path,
    hosts: (&Arc<Placer>, &Arc<Objects>, &Arc<MemoryStore>),
    namespace: &str,
    listener: uwe::run::Listener,
) -> Settings {
    let (placer, objects, runs) = hosts;
    let target = root.join("count.cwl");
    std::fs::write(
        &target,
        "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: [wc, -l]\n\
         inputs:\n  reads:\n    type: File\n    inputBinding: {position: 1}\n\
         stdout: count.txt\noutputs:\n  count: {type: stdout}\n",
    )
    .unwrap();
    let mut settings = Settings::new(target);
    settings.store = Some(root.join("store").to_string_lossy().into_owned());
    settings.cache_folder = Some(root.join("cache"));
    settings.output_dir = Some(root.join("out"));
    settings.setup = Some(Setup::default());
    settings.inputs = Some(json!({"reads": {"class": "File", "location": READS}}));
    settings.default_image = Some("counter:1".into());
    settings.host_executor = Some(HostExecutor::new(placer.clone()));
    settings.host_storage = vec![HostStorage::new(objects.clone())];
    settings.run_store = Some(runs.clone() as Arc<dyn RunStore>);
    settings.cache_namespace = Some(namespace.into());
    settings.listener = Some(listener);
    settings
}

#[tokio::test]
async fn bridges_native_host() {
    let root = tempfile::tempdir().unwrap();
    let placer = Arc::new(Placer {
        store: content_store::Store::local(&root.path().join("store")).unwrap(),
        scratch: root.path().join("host"),
        given: Mutex::default(),
    });
    let reads = b"@r1\nACGT\n+\nIIII\n".to_vec();
    let objects = Arc::new(Objects {
        objects: BTreeMap::from([(READS.to_owned(), reads.clone())]),
        downloads: AtomicUsize::new(0),
    });
    let runs = Arc::new(MemoryStore::new().unwrap());
    let (sender, heard) = mpsc::channel();
    let sender = Mutex::new(sender);
    let listener: uwe::run::Listener = Arc::new(move |event: Event| {
        let _ = sender.lock().unwrap().send(event);
    });
    let hosts = (&placer, &objects, &runs);
    let first = bridged(root.path(), hosts, "project:alpha", listener.clone());
    let run = RunRequest::new(first).run().await.unwrap();
    assert!(run.execution.error.is_none(), "{:?}", run.execution.error);
    let given = placer.given.lock().unwrap().clone();
    assert_eq!(given.len(), 1, "the host ran the task once");
    let [source] = given[0].sources.as_slice() else {
        panic!("one remote input: {:?}", given[0].sources);
    };
    assert_eq!(source.source, READS);
    assert_eq!(source.size, reads.len() as u64);
    assert_eq!(source.digest, Digest::sha256(&reads));
    assert_eq!(source.endpoints, REPLICAS);
    assert_eq!(source.reference.aruna_id.as_deref(), Some("01J9READS"));
    assert!(given[0].offline && given[0].cpus >= 1, "{:?}", given[0]);
    assert_eq!(
        objects.downloads.load(Ordering::SeqCst),
        0,
        "no input byte passed here"
    );
    let tasks = runs.tasks(&run.id).unwrap();
    assert_eq!((tasks.len(), tasks[0].state.as_str()), (1, "succeeded"));
    let (mut finished, mut logged) = (false, false);
    while let Ok(event) = heard.recv_timeout(Duration::from_secs(60)) {
        match event {
            Event::Task(task) if task.state == TaskState::Finished => finished = true,
            Event::Log { chunk, .. } => logged |= chunk.contains("proxy-a"),
            _ => {}
        }
        if finished && logged {
            break;
        }
    }
    assert!(
        finished && logged,
        "the listener heard the task and its output"
    );
    // The same namespace takes the result of the cache: no task runs and no byte moves.
    let second = bridged(root.path(), hosts, "project:alpha", listener.clone());
    let again = RunRequest::new(second).run().await.unwrap();
    assert!(
        again.execution.error.is_none(),
        "{:?}",
        again.execution.error
    );
    assert_eq!(
        placer.given.lock().unwrap().len(),
        1,
        "the cache answered the task"
    );
    assert_eq!(objects.downloads.load(Ordering::SeqCst), 0);
    let reused = runs.tasks(&again.id).unwrap();
    assert!(reused[0].reused.is_some(), "{:?}", reused[0]);
    // Another tenant does not see the result of the first one.
    let third = bridged(root.path(), hosts, "project:beta", listener);
    let other = RunRequest::new(third).run().await.unwrap();
    assert!(
        other.execution.error.is_none(),
        "{:?}",
        other.execution.error
    );
    assert_eq!(
        placer.given.lock().unwrap().len(),
        2,
        "the other tenant ran the task"
    );
    assert_eq!(objects.downloads.load(Ordering::SeqCst), 0);
}
