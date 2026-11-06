//! Backend configuration shared by the CLI and embedding API.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::arguments::Arguments;
use crate::repeat::Pinned;
use crate::setup::Setup;
use crate::{
    Failure, aws_batch, azure_batch, cache_directory, flyte, google_batch, grid, kubernetes, local,
    nomad, preview, resources, runs, slurm, tes,
};
use workflow_graph::{Attempt, ExecuteError, Executor, Files, Running, Simulator, TaskId};

/// Caps task requests and gives tasks without a time the default `time` in seconds, before
/// backend placement, retaining completion and cleanup ownership.
pub(crate) fn limited(
    executor: Box<dyn Executor>,
    cpus: Option<u32>,
    memory: Option<u64>,
    time: Option<u64>,
) -> Box<dyn Executor> {
    if cpus.is_none() && memory.is_none() && time.is_none() {
        return executor;
    }
    Box::new(Limited {
        executor,
        cpus,
        memory,
        time,
    })
}

struct Limited {
    executor: Box<dyn Executor>,
    cpus: Option<u32>,
    memory: Option<u64>,
    time: Option<u64>,
}

impl Executor for Limited {
    fn run(&mut self, mut attempt: Attempt) -> Running {
        let resources = &mut std::sync::Arc::make_mut(&mut attempt.task).resources;
        if let Some(cpus) = self.cpus {
            resources.cpus = Some(resources.cpus.unwrap_or(1.0).min(f64::from(cpus)));
        }
        if let Some(memory) = self.memory {
            resources.memory_bytes = Some(resources.memory_bytes.unwrap_or(memory).min(memory));
        }
        // Each executor then limits this time as a task's own, with its staging margin.
        resources.time_seconds = resources.time_seconds.or(self.time);
        self.executor.run(attempt)
    }

    fn settle(
        &mut self,
        task: TaskId,
        attempt: u32,
        accepted: bool,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), ExecuteError>> + Send + 'static>,
    > {
        self.executor.settle(task, attempt, accepted)
    }

    fn files(&self) -> Option<std::sync::Arc<dyn Files>> {
        self.executor.files()
    }

    fn hold(&mut self) {
        self.executor.hold();
    }

    fn release(&mut self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        self.executor.release()
    }
}

/// A real executor with the settings of the command line: its line per task, whether the cache
/// answers tasks, the share of cached tasks that run again for the reproducibility check, the
/// images that a repeat of a run pins, and which tasks get the network.
pub(crate) fn real<B: executor::Backend>(
    mut executor: task_runner::RunnerExecutor<B>,
    arguments: &Arguments,
    setup: &Setup,
    verbose: bool,
    reuse: bool,
    pinned: &Pinned,
    measured: &resources::Measures,
) -> Result<Box<dyn Executor>, Failure> {
    if pinned
        .images
        .keys()
        .any(|image| task_model::conda_image(image))
    {
        return Err(Failure::setup(
            "uwe: repeating historical Conda image pins is unsupported; run the workflow with current explicit Conda preparation inputs",
        ));
    }
    executor.verbose = verbose;
    executor.measured = measured.clone();
    executor.right_sizing = !arguments.no_right_sizing;
    executor.together = !arguments.no_grouping;
    executor.reuse = reuse;
    executor.check_reproducibility(arguments.check_reproducibility.unwrap_or(0.0));
    executor.pin_images(&pinned.images);
    executor.network = network_rule(arguments, setup);
    // A resumed run takes the results of its lineage, also of images without a digest.
    if arguments.resume.is_some() {
        let broken = |error: run_database::DatabaseError| Failure::setup(format!("uwe: {error}"));
        let database = runs::open(arguments)?;
        let run = database.run(executor.runner().run_id()).map_err(broken)?;
        if let Some(run) = run {
            let lineage = database.lineage(&run.lineage).map_err(broken)?;
            executor.resume_from(lineage.into_iter().collect());
        }
    }
    Ok(Box::new(executor))
}

/// Which tasks get the network: `[network]` of the config file, with `-no-network` for the
/// tasks whose workflow does not say.
pub(crate) fn network_rule(arguments: &Arguments, setup: &Setup) -> task_runner::NetworkRule {
    task_runner::NetworkRule {
        none_by_default: arguments.no_network || setup.network.none_by_default,
        ..setup.network.clone()
    }
}

/// An error when the run asks for tasks without network and its executor cannot cut a task
/// off the network yet: TES, Kubernetes, and SLURM with Singularity. It comes
/// before the executor starts, so that it is the first thing the user reads.
pub(crate) fn offline_possible(arguments: &Arguments, setup: &Setup) -> Result<(), Failure> {
    if !network_rule(arguments, setup).none_by_default {
        return Ok(());
    }
    let named = (arguments.slurm_container.as_deref())
        .or(setup.executors.slurm_container.as_deref())
        .unwrap_or("apptainer");
    let why = match arguments.executor.as_deref() {
        Some("tes") => "a TES server has no setting that cuts a task off the network",
        Some("kubernetes") => {
            "the Kubernetes executor makes no network policy that cuts a task off the network"
        }
        Some("aws-batch") => "AWS Batch has no setting that cuts a job off the network",
        Some("google-batch") => "Google Batch jobs need the network to stage their files",
        Some("azure-batch") => "Azure Batch tasks need the network to stage their files",
        Some("slurm") if named == "singularity" => {
            "Singularity cuts a task off the network only where the site allows it, which \
             uwe does not ask for (`-slurm-container apptainer`, `podman` or `docker` can)"
        }
        _ => return Ok(()),
    };
    Err(Failure::usage(format!(
        "uwe: {why}, so tasks cannot run without network there yet; leave out \
         `-no-network` and `default = \"none\"` of `[network]`, or use `-executor local`"
    )))
}

/// Builds the executor selected by `-executor`, defaulting to local containers.
/// Without `reuse`, real executors bypass cached results, including during replay.
pub(crate) fn executor(
    arguments: &Arguments,
    setup: &Setup,
    run: &str,
    verbose: bool,
    reuse: bool,
    pinned: &Pinned,
    measured: &resources::Measures,
) -> Result<Box<dyn Executor>, Failure> {
    let cache = cache_directory();
    let scratch = cache.join("scratch");
    offline_possible(arguments, setup)?;
    match arguments.executor.as_deref().unwrap_or("local") {
        "simulator" if arguments.check_reproducibility.is_some() => Err(Failure::usage(
            "uwe: the reproducibility check runs cached tasks again, so it needs a real \
             executor, like `-executor local`",
        )),
        "simulator" => {
            let mut simulator = Simulator::new(scratch.join("simulator").join(run));
            simulator.verbose = verbose;
            Ok(previewed(simulator, arguments, setup))
        }
        "local" => {
            let default_image = arguments.default_image.clone();
            let store = setup.store(arguments)?;
            let storage = setup.storage.clone();
            let engine = setup.engine();
            let mut executor =
                local::executor(store, &scratch, run, default_image, storage, &engine, setup)
                    .map_err(|message| Failure::setup(format!("uwe: {message}")))?;
            executor.priorities = std::sync::Arc::new(chains(arguments));
            real(executor, arguments, setup, verbose, reuse, pinned, measured)
        }
        "tes" => {
            let default_image = arguments.default_image.clone();
            let settings = &setup.executors;
            let server = (arguments.tes_server.as_deref())
                .or(settings.tes_server.as_deref())
                .unwrap_or(tes::DEFAULT_SERVER);
            let token = match std::env::var("UWE_TES_TOKEN") {
                Ok(token) if !token.is_empty() => Some(token),
                _ => (settings.tes_token.as_ref().map(|secret| secret.reveal()))
                    .transpose()
                    .map_err(|error| Failure::setup(format!("uwe: {error}")))?,
            };
            let store = setup.store(arguments)?;
            let storage = setup.storage.clone();
            let place = tes::Place {
                server,
                token,
                settings: &settings.tes,
                sites: &settings.tes_sites,
                scratch: &scratch,
            };
            let executor = tes::executor(store, place, run, default_image, storage, setup)
                .map_err(|message| Failure::setup(format!("uwe: {message}")))?;
            real(executor, arguments, setup, verbose, reuse, pinned, measured)
        }
        "slurm" => {
            let default_image = arguments.default_image.clone();
            let settings = &setup.executors;
            let named =
                (arguments.slurm_container.as_deref()).or(settings.slurm_container.as_deref());
            let container = slurm::container(named)
                .map_err(|message| Failure::usage(format!("uwe: {message}")))?;
            let options = match arguments.slurm_options.as_deref() {
                Some(options) => options.split_whitespace().map(str::to_owned).collect(),
                None => settings.slurm_options.clone().unwrap_or_default(),
            };
            let store = setup.store(arguments)?;
            let storage = setup.storage.clone();
            let mut backend = slurm::backend(&scratch, container, options, settings.slurm_pull);
            backend.remote = slurm::remote(settings, &storage)
                .map_err(|message| Failure::setup(format!("uwe: {message}")))?;
            backend.staging_time = settings.slurm_staging.margin();
            backend.cpu_memory_gb = settings.slurm_cpu_memory;
            if let Some(seconds) = settings.slurm_query {
                backend.query_grace = std::time::Duration::from_secs(seconds);
            }
            let executor = slurm::executor(store, backend, run, default_image, storage, setup)
                .map_err(|message| Failure::setup(format!("uwe: {message}")))?;
            real(executor, arguments, setup, verbose, reuse, pinned, measured)
        }
        "kubernetes" => {
            let default_image = arguments.default_image.clone();
            let settings = &setup.executors;
            let kubeconfig = (arguments.kubeconfig.as_deref()).or(settings.kubeconfig.as_deref());
            let namespace = (arguments.kubernetes_namespace.clone())
                .or_else(|| settings.kubernetes_namespace.clone());
            let store = setup.store(arguments)?;
            let executor = kubernetes::executor(
                store,
                &scratch,
                run,
                default_image,
                kubeconfig,
                namespace,
                setup,
            )
            .map_err(|message| Failure::setup(format!("uwe: {message}")))?;
            real(executor, arguments, setup, verbose, reuse, pinned, measured)
        }
        "aws-batch" => {
            let default_image = arguments.default_image.clone();
            let store = setup.store(arguments)?;
            let executor = aws_batch::executor(store, &scratch, run, default_image, setup)
                .map_err(|message| Failure::setup(format!("uwe: {message}")))?;
            real(executor, arguments, setup, verbose, reuse, pinned, measured)
        }
        "google-batch" => {
            let default_image = arguments.default_image.clone();
            let store = setup.store(arguments)?;
            let executor = google_batch::executor(store, &scratch, run, default_image, setup)
                .map_err(|message| Failure::setup(format!("uwe: {message}")))?;
            real(executor, arguments, setup, verbose, reuse, pinned, measured)
        }
        "azure-batch" => {
            let default_image = arguments.default_image.clone();
            let store = setup.store(arguments)?;
            let executor = azure_batch::executor(store, &scratch, run, default_image, setup)
                .map_err(|message| Failure::setup(format!("uwe: {message}")))?;
            real(executor, arguments, setup, verbose, reuse, pinned, measured)
        }
        "nomad" => {
            let default_image = arguments.default_image.clone();
            let store = setup.store(arguments)?;
            let executor = nomad::executor(store, &scratch, run, default_image, setup)
                .map_err(|message| Failure::setup(format!("uwe: {message}")))?;
            real(executor, arguments, setup, verbose, reuse, pinned, measured)
        }
        "htcondor" => {
            let default_image = arguments.default_image.clone();
            let store = setup.store(arguments)?;
            let executor = grid::htcondor(store, &scratch, run, default_image, setup)
                .map_err(|message| Failure::setup(format!("uwe: {message}")))?;
            real(executor, arguments, setup, verbose, reuse, pinned, measured)
        }
        "pbs" => {
            let default_image = arguments.default_image.clone();
            let store = setup.store(arguments)?;
            let executor = grid::pbs(store, &scratch, run, default_image, setup)
                .map_err(|message| Failure::setup(format!("uwe: {message}")))?;
            real(executor, arguments, setup, verbose, reuse, pinned, measured)
        }
        "flyte" => {
            let default_image = arguments.default_image.clone();
            let store = setup.store(arguments)?;
            let executor = flyte::executor(store, &scratch, run, default_image, setup)
                .map_err(|message| Failure::setup(format!("uwe: {message}")))?;
            real(executor, arguments, setup, verbose, reuse, pinned, measured)
        }
        other => Err(Failure::usage(format!(
            "uwe: unknown executor `{other}`; use `simulator`, `local`, `tes`, `slurm`, \
             `kubernetes`, `aws-batch`, `google-batch`, `azure-batch`, `nomad`, `htcondor`, \
             `pbs` or `flyte`"
        ))),
    }
}

/// For each step of the workflow, the time of the longest chain of work that follows from it
/// in its earlier real runs, so that the tasks on the longest remaining chain start first when
/// tasks wait for the machine. Empty before the first real run, or without a run database.
fn chains(arguments: &Arguments) -> std::collections::HashMap<String, u64> {
    let Some(workflow) = recorded_workflow(arguments) else {
        return Default::default();
    };
    (runs::open(arguments).ok())
        .and_then(|database| database.chains(&workflow).ok())
        .unwrap_or_default()
}

/// The workflow as the run database records it: the target as an absolute path.
pub(crate) fn recorded_workflow(arguments: &Arguments) -> Option<String> {
    let target = arguments.target.as_ref()?;
    let workflow = std::path::absolute(target).unwrap_or_else(|_| target.clone());
    Some(workflow.to_string_lossy().into_owned())
}

/// Previews cached tasks using recorded pins for mutable images and simulates cache misses.
fn previewed(simulator: Simulator, arguments: &Arguments, setup: &Setup) -> Box<dyn Executor> {
    // A real run without the cache would run every task.
    if arguments.no_cache {
        return Box::new(simulator);
    }
    let pins = run_database::Database::read_only(&runs::database_path(arguments))
        .and_then(|database| database.pins())
        .unwrap_or_default();
    let location = setup.store_location(arguments);
    if matches!(setup.storage.store(&location), Ok(storage::StoreLocation::Folder(folder)) if !folder.is_dir())
        || setup
            .storage
            .local_store()
            .is_some_and(|folder| !std::path::Path::new(folder).is_dir())
    {
        return Box::new(simulator);
    }
    let Ok(store) = setup.store(arguments) else {
        return Box::new(simulator);
    };
    let settings = task_runner::Settings {
        downloads: cache_directory().join("downloads"),
        import_inputs: false,
        storage: setup.storage.clone(),
        digests: None,
    };
    let mut lookup = task_runner::Lookup::new(store, settings);
    lookup.references(setup.references.clone());
    lookup.network(network_rule(arguments, setup));
    let default_image = arguments.default_image.clone();
    Box::new(preview::Previewed::new(
        simulator,
        lookup,
        pins,
        default_image,
    ))
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::sync::{Arc, Mutex};

    use workflow_graph::{Attempt, CancellationToken, Executor, Outcome, Resources, Running, Task};

    struct Observed(Arc<Mutex<Resources>>);

    #[test]
    fn preserves_preview_files() {
        for database_exists in [false, true] {
            for store_exists in [false, true] {
                let root = tempfile::tempdir().unwrap();
                let _scope = crate::run::support::CacheScope::new(false);
                let cache = root.path().join("cache");
                crate::run::support::use_cache_folder(cache.clone());
                let path = root.path().join("database/runs.sqlite");
                let _database =
                    database_exists.then(|| run_database::Database::open(&path).unwrap());
                let store = root.path().join("store");
                if store_exists {
                    std::fs::create_dir(&store).unwrap();
                }
                let arguments = crate::arguments::Arguments {
                    database: Some(path.clone()),
                    store: Some(store.to_string_lossy().into_owned()),
                    ..Default::default()
                };
                let _executor = super::previewed(
                    workflow_graph::Simulator::new(root.path().join("simulator")),
                    &arguments,
                    &crate::setup::Setup::default(),
                );
                assert_eq!(path.exists(), database_exists);
                assert_eq!(store.exists(), store_exists);
                assert!(!cache.exists(), "preview must not open the digest database");
                if store_exists {
                    assert_eq!(std::fs::read_dir(&store).unwrap().count(), 0);
                }
            }
        }
    }

    #[test]
    fn previews_pinned_images() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("database/runs.sqlite");
        let database = run_database::Database::open(&path).unwrap();
        drop(database);
        let before = std::fs::read(&path).unwrap();
        let store = content_store::Store::local(&root.path().join("store")).unwrap();
        let task = Task {
            id: workflow_graph::TaskId(1),
            image: Some(format!("image@sha256:{}", "a".repeat(64))),
            command: vec!["true".into()],
            network: workflow_graph::Network::Denied,
            ..Default::default()
        };
        let request = task_runner::request(&task, task.image.clone(), Vec::new(), None);
        let key = request.spec.cache_key().unwrap();
        let entry = task_runner::CacheEntry {
            run: "prior".into(),
            task: "task".into(),
            exit_code: 0,
            files: Vec::new(),
            stdout: None,
            stderr: None,
            milliseconds: None,
            containers: Vec::new(),
        };
        workflow_graph::runtime()
            .unwrap()
            .block_on(store.put_entry(
                &request.spec.namespace,
                &key,
                entry.to_bytes(&key).unwrap().into(),
            ))
            .unwrap();
        let task = Arc::new(task);
        for database in [path.clone(), root.path().join("missing/runs.sqlite")] {
            let arguments = crate::arguments::Arguments {
                database: Some(database),
                store: Some(root.path().join("store").to_string_lossy().into_owned()),
                ..Default::default()
            };
            let mut executor = super::previewed(
                workflow_graph::Simulator::new(root.path().join("simulator")),
                &arguments,
                &crate::setup::Setup::default(),
            );
            let outcome = workflow_graph::runtime()
                .unwrap()
                .block_on(executor.run(Attempt {
                    task: task.clone(),
                    number: 1,
                    cancel: CancellationToken::new(),
                }))
                .unwrap();
            assert!(outcome.cached);
        }
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
        assert!(!root.path().join("missing").exists());
    }

    impl Executor for Observed {
        fn run(&mut self, attempt: Attempt) -> Running {
            *self.0.lock().unwrap() = attempt.task.resources.clone();
            Running::ended(Ok(Outcome::default()))
        }

        fn release(&mut self) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send>> {
            self.0.lock().unwrap().cpus = Some(-1.0);
            Box::pin(async {})
        }
    }

    #[tokio::test]
    async fn forwards_daemon_release() {
        let observed = Arc::new(Mutex::new(Resources::default()));
        let mut executor =
            super::limited(Box::new(Observed(observed.clone())), Some(2), None, None);
        executor.release().await;
        assert_eq!(observed.lock().unwrap().cpus, Some(-1.0), "released");
    }

    #[tokio::test]
    async fn caps_backend_requests() {
        for (resources, cpus) in [
            (
                Resources {
                    cpus: Some(4.0),
                    memory_bytes: Some(32 << 20),
                    ..Resources::default()
                },
                2.0,
            ),
            (Resources::default(), 1.0),
        ] {
            let observed = Arc::new(Mutex::new(Resources::default()));
            let mut executor = super::limited(
                Box::new(Observed(observed.clone())),
                Some(2),
                Some(16 << 20),
                None,
            );
            executor
                .run(Attempt {
                    task: Arc::new(Task {
                        resources,
                        ..Task::default()
                    }),
                    number: 1,
                    cancel: CancellationToken::new(),
                })
                .await
                .unwrap();
            let observed = observed.lock().unwrap();
            assert_eq!(observed.cpus, Some(cpus));
            assert_eq!(observed.memory_bytes, Some(16 << 20));
        }
    }

    /// The time that a task gets: the default only when it sets none.
    fn timed(own: Option<u64>, default: Option<u64>) -> Arc<Task> {
        let observed = Arc::new(Mutex::new(Resources::default()));
        let mut executor =
            super::limited(Box::new(Observed(observed.clone())), None, None, default);
        let resources = Resources {
            time_seconds: own,
            ..Resources::default()
        };
        let task = Arc::new(Task {
            command: vec!["true".into()],
            resources,
            ..Task::default()
        });
        let attempt = Attempt {
            task: task.clone(),
            number: 1,
            cancel: CancellationToken::new(),
        };
        workflow_graph::runtime()
            .unwrap()
            .block_on(executor.run(attempt))
            .unwrap();
        let resources = observed.lock().unwrap().clone();
        Arc::new(Task {
            resources,
            ..Task::clone(&task)
        })
    }

    #[test]
    fn defaults_task_time() {
        assert_eq!(timed(None, Some(90)).resources.time_seconds, Some(90));
        assert_eq!(timed(Some(30), Some(90)).resources.time_seconds, Some(30));
        assert_eq!(timed(None, None).resources.time_seconds, None);
    }

    /// A TES server whose task runs until it is cancelled, like one that never tells its end;
    /// it keeps the task document it got.
    fn running_server(sent: Arc<Mutex<Option<serde_json::Value>>>) -> String {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let mut state = "RUNNING";
            for mut stream in listener.incoming().map_while(Result::ok) {
                let mut reader = BufReader::new(&mut stream);
                let (mut first, mut length) = (String::new(), 0);
                reader.read_line(&mut first).unwrap();
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    match line.split_once(':') {
                        _ if line.trim().is_empty() => break,
                        Some((name, value)) if name.eq_ignore_ascii_case("content-length") => {
                            length = value.trim().parse().unwrap();
                        }
                        _ => {}
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                if first.starts_with("POST /ga4gh/tes/v1/tasks ") {
                    *sent.lock().unwrap() = serde_json::from_slice(&body).ok();
                } else if first.contains(":cancel") {
                    state = "CANCELED";
                }
                let answer = format!(r#"{{"id":"task-1","state":"{state}"}}"#);
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                    answer.len()
                );
                stream.write_all(reply.as_bytes()).unwrap();
            }
        });
        address
    }

    #[test]
    fn tes_task_timed() {
        use executor::Backend;
        let root = tempfile::tempdir().unwrap();
        let task = timed(None, Some(1));
        let request = task_runner::request(&task, Some("test".into()), Vec::new(), None);
        assert_eq!(request.placement.time_seconds, Some(1));
        let sent = Arc::new(Mutex::new(None));
        let address = running_server(sent.clone());
        let config = executor::registry::ImageConfig::default();
        let backend = tes_executor::TesBackend {
            client: tes_executor::Client::new(&address, None).unwrap(),
            scratch: root.path().join("scratch"),
            staging: None,
            // Without a staging margin the task stops right after its time.
            settings: tes_executor::Settings {
                staging_minutes: Some(0),
                ..Default::default()
            },
            configs: executor::registry::ImageConfigs::known([("test".into(), config)]),
        };
        let staging = executor::Staging::default();
        let job = executor::Job {
            name: "run-P-1-1",
            spec: &request.spec,
            staging: &staging,
            placement: &request.placement,
        };
        let finished = workflow_graph::runtime()
            .unwrap()
            .block_on(backend.run(job, &CancellationToken::new()))
            .unwrap();
        assert_eq!(finished.exit_code, Some(143));
        let stderr = std::fs::read_to_string(finished.stderr()).unwrap();
        assert!(stderr.contains("after its time of 1 seconds"), "{stderr}");
        let sent = sent.lock().unwrap().clone().unwrap();
        let command = sent["executors"][0]["command"][2].as_str().unwrap();
        assert!(command.contains("( sleep 1 && kill"), "{command}");
    }
}
