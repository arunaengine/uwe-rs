//! Routes the tasks of one run across backends of different kinds by site, capability and data.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use content_store::Store;
use executor::manifest::Collected;
use executor::{Backend, DynBackend, ExecutorError, Finished, Job, Live, LiveFeed, LogStream};
use task_runner::{Runner, RunnerExecutor, Settings, Site, SiteRule, Sites};
use tokio_util::sync::CancellationToken;
use workflow_graph::{Attempt, Executor, Input, Resources, Task, TaskId};

/// Writes `out.txt` and remembers which attempts it ran; it says its name on standard output
/// while the attempt runs, as a backend with live output does.
struct Cluster {
    scratch: PathBuf,
    ran: Arc<Mutex<Vec<String>>>,
}

/// Another kind of backend, which runs like a cluster under its own name.
struct Grid(Cluster);

impl Cluster {
    fn finish(&self, job: Job<'_>, said: &str) -> Result<Finished, ExecutorError> {
        self.ran.lock().unwrap().push(job.name.to_owned());
        if let Some(live) = &job.staging.live {
            live.log(LogStream::Stdout, &format!("{said}\n"));
        }
        let finished = Finished {
            exit_code: Some(0),
            files: vec![Collected {
                path: "out.txt".into(),
                directory: false,
            }],
            root: self.scratch.join(job.name),
            usage: None,
            address: None,
            receipts: Vec::new(),
        };
        std::fs::create_dir_all(finished.root.join("control"))?;
        std::fs::create_dir_all(finished.root.join("work"))?;
        std::fs::write(finished.root.join("control/exit"), "0\n")?;
        std::fs::write(finished.local_path("out.txt"), said)?;
        Ok(finished)
    }
}

impl Backend for Cluster {
    async fn pin(&self, image: &str) -> Result<Option<String>, ExecutorError> {
        Ok(Some(format!("{image}@sha256:00")))
    }

    async fn run(&self, job: Job<'_>, _: &CancellationToken) -> Result<Finished, ExecutorError> {
        self.finish(job, "cluster")
    }
}

impl Backend for Grid {
    async fn pin(&self, image: &str) -> Result<Option<String>, ExecutorError> {
        Ok(Some(format!("{image}@sha256:00")))
    }

    async fn run(&self, job: Job<'_>, _: &CancellationToken) -> Result<Finished, ExecutorError> {
        self.0.finish(job, "grid")
    }
}

fn task(id: u64, input: &Path, resources: Resources, site: Option<&str>) -> Task {
    Task {
        id: TaskId(id),
        name: format!("T ({id})"),
        step: "T".into(),
        image: Some("debian".into()),
        command: vec!["true".into(), id.to_string()],
        inputs: vec![Input {
            path: "in.txt".into(),
            source: input.to_string_lossy().into_owned(),
            ..Input::default()
        }],
        outputs: vec!["out.txt".into()],
        resources,
        site: site.map(str::to_owned),
        ..Task::default()
    }
}

#[tokio::test]
async fn routes_across_backends() {
    let root = tempfile::tempdir().unwrap();
    let near = root.path().join("hamburg");
    std::fs::create_dir_all(&near).unwrap();
    let (here, there) = (root.path().join("in.txt"), near.join("in.txt"));
    std::fs::write(&here, "here\n").unwrap();
    std::fs::write(&there, "there\n").unwrap();
    let ran: BTreeMap<&str, Arc<Mutex<Vec<String>>>> = ["local", "gpu", "hamburg"]
        .into_iter()
        .map(|name| (name, Arc::default()))
        .collect();
    let cluster = |name: &str| Cluster {
        scratch: root.path().join("scratch").join(name),
        ran: ran[name].clone(),
    };
    let sites: Sites<Arc<dyn DynBackend>> = Sites {
        default: Arc::new(cluster("local")),
        sites: [
            (
                "gpu".to_owned(),
                Arc::new(Grid(cluster("gpu"))) as Arc<dyn DynBackend>,
            ),
            ("hamburg".to_owned(), Arc::new(cluster("hamburg")) as _),
        ]
        .into(),
    };
    let settings = Settings {
        downloads: root.path().join("downloads"),
        import_inputs: false,
        storage: storage::Storage::default(),
        digests: None,
    };
    let store = Store::local(&root.path().join("store")).unwrap();
    let runner = Runner::new(store, sites, "run1", settings);
    let mut executor = RunnerExecutor::new(runner, None);
    executor.sites = SiteRule {
        sites: vec![
            Site {
                name: "gpu".into(),
                gpus: true,
                max_memory: Some(16 << 30),
                ..Site::default()
            },
            Site {
                name: "hamburg".into(),
                near: vec![near.to_string_lossy().into_owned()],
                max_memory: Some(64 << 30),
                ..Site::default()
            },
        ],
        default: Site {
            max_memory: Some(8 << 30),
            ..Site::default()
        },
    };
    let heard = Arc::new(Mutex::new(Vec::new()));
    let kept = heard.clone();
    executor.use_live(LiveFeed::new(move |task, attempt, live| {
        kept.lock().unwrap().push((task, attempt, live));
    }));
    let gpus = Resources {
        gpus: Some(1),
        ..Resources::default()
    };
    let large = Resources {
        memory_bytes: Some(32 << 30),
        ..Resources::default()
    };
    let tasks = [
        task(1, &here, Resources::default(), None),
        task(2, &here, gpus, None),
        task(3, &there, Resources::default(), None),
        task(4, &here, large, None),
        task(5, &there, Resources::default(), Some("gpu")),
    ];
    for task in tasks {
        let attempt = Attempt {
            task: Arc::new(task),
            number: 1,
            cancel: CancellationToken::new(),
        };
        let outcome = executor.run(attempt).await.expect("runs");
        assert_eq!(outcome.exit_code, 0);
    }
    let names = |site: &str| ran[site].lock().unwrap().clone();
    assert_eq!(names("local"), ["run1-T-1-1"]);
    assert_eq!(
        names("gpu"),
        ["run1-T-2-1", "run1-T-5-1"],
        "by GPU and by name"
    );
    assert_eq!(
        names("hamburg"),
        ["run1-T-3-1", "run1-T-4-1"],
        "by data and by memory"
    );
    let heard = heard.lock().unwrap();
    let said = |task: u64| {
        (heard.iter())
            .find(|(id, attempt, _)| *id == task && *attempt == 1)
            .map(|(_, _, live)| live.clone())
    };
    let log = |text: &str| Live::Log {
        stream: LogStream::Stdout,
        chunk: format!("{text}\n"),
    };
    assert_eq!(said(2), Some(log("grid")));
    assert_eq!(said(3), Some(log("cluster")));
}

#[test]
fn prefers_near_endpoints() {
    let rule = SiteRule {
        sites: vec![
            Site {
                name: "first".into(),
                endpoints: vec!["proxy-a".into()],
                ..Site::default()
            },
            Site {
                name: "second".into(),
                endpoints: vec!["proxy-b".into()],
                ..Site::default()
            },
        ],
        ..SiteRule::default()
    };
    assert!(rule.uses_endpoints());
    let source = "aruna://object/1";
    let placed = Task {
        inputs: vec![Input {
            path: "in".into(),
            source: source.into(),
            ..Input::default()
        }],
        ..Task::default()
    };
    let held = [(source.to_owned(), vec!["proxy-b".into(), "proxy-a".into()])].into();
    assert_eq!(rule.choose_near(&placed, &held), Ok(Some("second")));
    assert_eq!(rule.choose(&placed), Ok(None), "no endpoints, no site");
    let gpu = Task {
        resources: Resources {
            gpus: Some(2),
            ..Resources::default()
        },
        ..placed
    };
    let refused = rule.choose_near(&gpu, &held);
    assert!(
        refused.is_err(),
        "no site runs GPU tasks, nor does the executor: {refused:?}"
    );
}
