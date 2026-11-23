//! Complete workflow runs, including recording and resume.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub(crate) mod backend;
mod context;
pub(crate) mod driver;
mod events;
mod settings;
pub(crate) mod support;

pub use backend::HostExecutor;
pub use config_file::SecretValues;
pub use context::{HostRuns, HostStorage, RunContext};
pub use events::{Event, Listener, LogStream, PrintSink, TaskEvent, TaskState};
pub use executor::orphans;
pub use settings::Settings;
pub use task_runner::NetworkRule;
pub use workflow_graph::Language;

use crate::application::{self, Description, Policy};
use crate::arguments::{Arguments, Resume};
use crate::repeat::Pinned;
use crate::{
    Failure, driver, executor, failure, fetched, inputs, launch, network_rule, page, progress,
    record, report, resources, resume, run_id, sources, stop_on_interrupt, target_and_language,
    terminal,
};
use run_database::Recorder;
use run_page::Server;
use workflow_graph::{Engine, Execution, Order, RunOptions};

/// The description of a workflow from a driver that plans it and starts no task.
fn described(
    arguments: &Arguments,
    setup: &crate::setup::Setup,
    (target, language): (std::path::PathBuf, Language),
    cancel: &workflow_graph::CancellationToken,
) -> Result<Description, Failure> {
    let launch = launch(arguments, setup, target);
    let mut driver = driver(language, &launch, &run_id(), true, arguments, setup, cancel)?;
    application::describe(driver.as_mut()).map_err(|error| failure(error.to_string()))
}

/// A workflow submission using embedding settings and the shared CLI execution path.
/// Strict admission checks typed JSON without CLI coercion or simulated assumptions.
pub struct RunRequest {
    pub settings: Settings,
    pub policy: Policy,
}

impl RunRequest {
    /// Selects a configured workflow with strict typed JSON admission.
    pub fn new(settings: Settings) -> Self {
        Self {
            settings,
            policy: Policy::Strict,
        }
    }

    /// Runs safely inside Tokio. Drivers and the blocking engine stay on the worker thread.
    /// Dropping this future requests cancellation; the worker continues cleanup and recording.
    /// Await completion after cancelling the token to observe the final result.
    pub async fn run(mut self) -> Result<Run, Failure> {
        let cancel = self.settings.cancel.child_token();
        let _stop = cancel.clone().drop_guard();
        self.settings.cancel = cancel;
        tokio::task::spawn_blocking(move || self.run_blocking())
            .await
            .map_err(|error| Failure::setup(format!("uwe: run worker failed: {error}")))?
    }

    /// Blocking equivalent of `run`. Call outside an async runtime.
    /// Workflow failures are retained in `Run::execution`, together with the partial graph.
    pub fn run_blocking(mut self) -> Result<Run, Failure> {
        let quiet = !self.settings.print;
        // The pins of the host take the way of the pins of a repeat.
        let pinned = Pinned {
            images: std::mem::take(&mut self.settings.pinned_images),
            ..Pinned::default()
        };
        execute_with(self.settings, quiet, &pinned, Some(self.policy))
    }

    /// Builds and describes the configured driver without starting tasks or recording a run.
    /// Native planning can still read files and use configured container helpers.
    pub async fn describe(self) -> Result<Description, Failure> {
        let cancel = self.settings.cancel.child_token();
        let _stop = cancel.clone().drop_guard();
        tokio::task::spawn_blocking(move || {
            let _cache = support::CacheScope::new(false);
            let (arguments, setup, _inputs) = self.ready()?;
            let _registry = setup.registry.enter().map_err(Failure::setup)?;
            let (target, language) = target_and_language(&arguments, &setup)?;
            described(&arguments, &setup, (target, language), &cancel)
        })
        .await
        .map_err(|error| Failure::setup(format!("uwe: discovery worker failed: {error}")))?
    }

    /// The support report of the configured workflow: which parts uwe compiles into wir, runs
    /// as native blocks or refuses, with their images. Like `describe`, it starts no task.
    pub async fn support(self) -> Result<crate::support::Report, Failure> {
        let cancel = self.settings.cancel.child_token();
        let _stop = cancel.clone().drop_guard();
        tokio::task::spawn_blocking(move || {
            let _cache = support::CacheScope::new(false);
            let (arguments, setup, _inputs) = self.ready()?;
            let _registry = setup.registry.enter().map_err(Failure::setup)?;
            let (target, language) = target_and_language(&arguments, &setup)?;
            let given = (target.as_path(), language);
            Ok(crate::support::report(&arguments, &setup, given, &cancel))
        })
        .await
        .map_err(|error| Failure::setup(format!("uwe: support worker failed: {error}")))?
    }

    /// `describe` and `support` together, which find the setup and the workflow once.
    pub(crate) async fn overview(self) -> Result<(Description, crate::support::Report), Failure> {
        let cancel = self.settings.cancel.child_token();
        let _stop = cancel.clone().drop_guard();
        tokio::task::spawn_blocking(move || {
            let _cache = support::CacheScope::new(false);
            let (arguments, setup, _inputs) = self.ready()?;
            let _registry = setup.registry.enter().map_err(Failure::setup)?;
            let (target, language) = target_and_language(&arguments, &setup)?;
            let given = (target.clone(), language);
            let description = described(&arguments, &setup, given, &cancel)?;
            let given = (target.as_path(), language);
            let report = crate::support::report(&arguments, &setup, given, &cancel);
            Ok((description, report))
        })
        .await
        .map_err(|error| Failure::setup(format!("uwe: discovery worker failed: {error}")))?
    }

    /// The arguments and the setup of the settings, with the folders, the storage providers
    /// of the host and the network rule in place, as describing a workflow needs them.
    fn ready(
        mut self,
    ) -> Result<
        (
            Arguments,
            crate::setup::Setup,
            Option<tempfile::NamedTempFile>,
        ),
        Failure,
    > {
        let network = self.settings.network.take();
        let hosts = std::mem::take(&mut self.settings.host_storage);
        let (arguments, setup, inputs) = self.settings.prepare()?;
        let mut setup = match setup {
            Some(setup) => setup,
            None => crate::setup::Setup::load(&arguments)?,
        };
        setup.use_folders(&arguments);
        setup.storage = HostStorage::added(&hosts, &setup.storage.session());
        if let Some(network) = network {
            setup.network = network;
        }
        Ok((arguments, setup, inputs))
    }

    /// The remote jobs that the run `settings.run_id` left on any remote executor, with what
    /// their servers say of them. Changes nothing. The settings give the config file, the
    /// cache, the journal store and the secrets of the run.
    pub async fn remote_tasks(self) -> Result<Vec<orphans::Orphan>, Failure> {
        let settings = self.settings;
        tokio::task::spawn_blocking(move || remote_tasks(settings, false))
            .await
            .map_err(|error| Failure::setup(format!("uwe: task worker failed: {error}")))?
    }

    /// Cancels the remote tasks that the run `settings.run_id` left and waits until their
    /// servers confirm that they ended. It keeps the tasks of an attempt whose uwe process
    /// still runs.
    pub async fn stop_tasks(self) -> Result<Vec<orphans::Orphan>, Failure> {
        let settings = self.settings;
        tokio::task::spawn_blocking(move || remote_tasks(settings, true))
            .await
            .map_err(|error| Failure::setup(format!("uwe: task worker failed: {error}")))?
    }
}

/// The remote tasks of the run of `settings`, found with the config, cache and journal store
/// that a run with these settings uses, and stopped with `stop`. Each remote executor with a
/// journal of the run builds its backend, which reports or stops its jobs.
fn remote_tasks(settings: Settings, stop: bool) -> Result<Vec<orphans::Orphan>, Failure> {
    let _cache = support::CacheScope::new(false);
    let run = (settings.run_id.clone())
        .ok_or_else(|| Failure::usage("uwe: set run_id to the run whose remote tasks to find"))?;
    settings::check_id(&run)?;
    let cache = match &settings.hosted {
        Some(hosted) => Some(hosted.check(settings.preset.as_deref())?.join("cache")),
        None => settings.cache_folder,
    };
    let mut arguments = Arguments {
        config_file: settings.config_file,
        executor: settings.executor,
        store: settings.store,
        secrets: settings.secrets,
        cache_folder: cache.clone(),
        host_executor: settings.host_executor,
        ..Arguments::default()
    };
    let mut setup = match settings.setup {
        Some(setup) => setup,
        None => {
            crate::setup::Defaults::load(&arguments)?.apply("run", &mut arguments)?;
            crate::setup::Setup::load(&arguments)?
        }
    };
    setup.use_folders(&arguments);
    if let Some(folder) = cache {
        crate::use_cache_folder(folder);
    }
    if settings.journal_store.is_some() {
        setup.journal_store = settings.journal_store;
    }
    let failed = |error: executor::ExecutorError| Failure::setup(format!("uwe: {error}"));
    let store = setup.context().journal_store;
    // TES, Kubernetes and SLURM keep their journals beside their files in the scratch folder.
    let files = executor::FileJournal::new(setup.cache_directory().join("scratch"));
    let runtime = workflow_graph::runtime().map_err(|error| failure(format!("uwe: {error}")))?;
    let mut found = Vec::new();
    let names = (backend::REMOTE.iter().copied())
        .chain(arguments.host_executor.is_some().then_some(backend::HOST));
    for name in names {
        // Without a journal no server needs to answer.
        let journaled = |store: &dyn executor::JournalStore| store.list(name, &run);
        if name != backend::HOST
            && journaled(store.as_ref()).map_err(failed)?.is_empty()
            && journaled(&files).map_err(failed)?.is_empty()
        {
            continue;
        }
        let backend = backend::dynamic(name, &arguments, &setup, &run)?;
        let answer = match stop {
            true => runtime.block_on(backend.stop_orphans(&run)),
            false => runtime.block_on(backend.orphans(&run)),
        };
        found.extend(answer.map_err(failed)?);
    }
    Ok(found)
}

/// Runs the target with its language's driver and the chosen executor, and records the run
/// in the run database. `quiet` keeps standard output free for the graph. With `-page`, the
/// page of the run serves from the start; the result holds it.
pub(crate) fn execute(arguments: &Arguments, quiet: bool, pinned: &Pinned) -> Result<Ran, Failure> {
    let mut settings = arguments.run_settings();
    settings.cancel = stop_on_interrupt();
    execute_with(settings, quiet, pinned, None)
}

fn execute_with(
    mut settings: Settings,
    quiet: bool,
    pinned: &Pinned,
    admission: Option<Policy>,
) -> Result<Run, Failure> {
    let _cache = support::CacheScope::new(settings.cli.is_some());
    let hosted = settings.hosted.clone();
    let strict = settings.strict_provenance || hosted.is_some();
    let admission = if hosted.is_some() {
        Some(Policy::Strict)
    } else {
        admission
    };
    let interrupt = if settings.cli.is_some() {
        settings.cancel.clone()
    } else {
        settings.cancel.child_token()
    };
    let _stop = interrupt.clone().drop_guard();
    let network = settings.network.take();
    let cpus = settings.cpus;
    let memory = settings.memory_bytes;
    let chosen = settings.run_id.take();
    let listener = settings.listener.take();
    let tell = |event: Event| {
        if let Some(listener) = &listener {
            listener(event);
        }
    };
    if let Some(id) = &chosen {
        settings::check_id(id)?;
    }
    let pool = settings.local_pool.clone();
    let hosts = std::mem::take(&mut settings.host_storage);
    let journals = settings.journal_store.take();
    let (mut arguments, setup, _inputs) = settings.prepare()?;
    if network.is_some() {
        arguments.no_network = false;
    }
    let arguments = &arguments;
    // A second insert of the id would fail too, but only after the workflow was fetched.
    let known = |id: &str| -> Result<bool, Failure> {
        let found = match &arguments.run_store {
            Some(host) => host.0.run(id),
            None => crate::runs::open(arguments)?.run(id),
        };
        Ok(found
            .map_err(|error| Failure::setup(format!("uwe: {error}")))?
            .is_some())
    };
    if let Some(id) = &chosen
        && known(id)?
    {
        return Err(Failure::usage(format!(
            "uwe: a run with the id {id} is already in the run database"
        )));
    }
    let mut setup = match setup {
        Some(setup) => setup,
        None => crate::setup::Setup::load(arguments)?,
    };
    if pool.is_some() {
        setup.local_pool = pool;
    }
    if journals.is_some() {
        setup.journal_store = journals;
    }
    setup.variables = arguments.secrets.clone();
    setup.use_folders(arguments);
    let _registry = setup.registry.enter().map_err(Failure::setup)?;
    setup.storage = HostStorage::added(&hosts, &setup.storage.session());
    // A repeat reads the remote files of the run it repeats as they were, also when the
    // workflow compiles.
    for file in &pinned.files {
        setup.storage.pin(file.clone());
    }
    let (target, language) = target_and_language(arguments, &setup)?;
    let run = chosen.unwrap_or_else(run_id);
    // A run compiles its workflow into wir and checks its inputs with the schema of the
    // workflow; a wir document and a Galaxy tool or workflow run as they are.
    let direct = matches!(language, Language::Wir | Language::Galaxy);
    let source = (!direct).then(|| (launch(arguments, &setup, target.clone()), language));
    // The run records the workflow and the command line that the user gave.
    let given = arguments;
    let converted;
    // The files of the values of a compiled run, removed when the run ends.
    let private;
    let (target, language, arguments) = match source.is_some() {
        true => {
            let crate::lower::Compiled {
                path,
                inputs,
                private: values,
                secret: secret_inputs,
                resumes,
                launch: launch_run,
            } = crate::lower::document(&target, language, arguments, &setup, &run)?;
            private = values;
            // A WDL input file and parameters become one file for the entry graph, or values in
            // memory; Snakemake config files and values are in the document.
            let compiled = matches!(language, Language::Snakemake | Language::Wdl);
            let params = match inputs {
                Some(_) => Vec::new(),
                None if compiled => Vec::new(),
                None => arguments.params.clone(),
            };
            converted = Arguments {
                inputs: inputs.or(arguments.inputs.clone().filter(|_| !compiled)),
                params,
                secret_inputs,
                resume: resumes.map(Resume::Run).or(arguments.resume.clone()),
                launch_run,
                ..arguments.clone()
            };
            (path, Language::Wir, &converted)
        }
        false => {
            private = Vec::new();
            (target, language, arguments)
        }
    };
    let private: Vec<crate::lower::Values> = private;
    let target = &target;
    let launch = launch(arguments, &setup, target.clone());
    // The config of a Nextflow pipeline can pick the executor and the store of its run.
    let (first, written) = match &source {
        Some((launch, language)) => (launch, *language),
        None => (&launch, language),
    };
    let picked;
    let arguments = match crate::nextflow::pipeline_executor(written, first, arguments, &mut setup)?
    {
        Some(executor) => {
            picked = Arguments {
                executor: Some(executor),
                executor_default: false,
                ..arguments.clone()
            };
            &picked
        }
        None => arguments,
    };
    // nf-cloudcache puts the store where the config says, unless `-store` names one.
    let cached;
    let arguments = match written {
        Language::Nextflow if arguments.store.is_none() => {
            match crate::nextflow::cloud_cache(first, arguments, &setup).map_err(Failure::setup)? {
                Some(location) => {
                    cached = Arguments {
                        store: Some(location),
                        ..arguments.clone()
                    };
                    &cached
                }
                None => arguments,
            }
        }
        _ => arguments,
    };
    let name = arguments.executor.as_deref().unwrap_or("local");
    if let Some(network) = network {
        setup.network = network;
    }
    setup.conda = setup
        .conda
        .as_ref()
        .map(|config| config.for_network(network_rule(arguments, &setup).none_by_default))
        .transpose()
        .map_err(|error| Failure::setup(error.to_string()))?;
    let store = setup.store_location(arguments);
    let real = name != "simulator";
    // A real run shows its steps as it goes; in a terminal they are drawn in place, so the
    // executors print no line per task there.
    let shown = real && !quiet;
    let verbose = !quiet && !(shown && terminal::possible());
    let runtime = workflow_graph::runtime().map_err(|error| failure(format!("uwe: {error}")))?;
    let recorded = Arguments {
        resume: arguments.resume.clone(),
        ..given.clone()
    };
    let (workflow, written) = match &source {
        Some((launch, language)) => (&launch.target, *language),
        None => (target, language),
    };
    let mut sources = runtime.block_on(sources::collect(workflow, given));
    sources.leave_out(&private);
    if real && strict {
        sources.complete().map_err(Failure::setup)?;
    }
    let invocation = admission.map(|_| {
        let mut words = record::run_arguments(&recorded);
        for (option, targets) in [
            ("-until", &arguments.until),
            ("-omit-from", &arguments.omit_from),
            ("-forcerun", &arguments.forcerun),
            ("-no-cache-steps", &arguments.no_cache_steps),
        ] {
            for target in targets {
                words.extend([option.into(), target.clone()]);
            }
        }
        if arguments.forceall {
            words.push("-forceall".into());
        }
        // A host turns the network off through the setup; a repeat of the run must too.
        if setup.network.none_by_default && !words.iter().any(|word| word == "-no-network") {
            words.push("-no-network".into());
        }
        words
    });
    let started = record::Started {
        id: &run,
        target: workflow,
        language: written,
        executor: name,
        simulated: !real,
        store: real.then_some(store.as_str()),
        sources: &sources,
        repeats: pinned.repeats.as_deref(),
        invocation: invocation.as_deref(),
        strict_provenance: strict,
        document: source.is_some().then_some(target.as_path()),
    };
    // Read before this run is in the database.
    let measured = resources::measured(arguments);
    let record::Recording {
        mut recorder,
        reusable,
    } = record::start(&recorded, &started)?;
    let page = match arguments.page {
        true => Some(page::start(arguments, &format!("/runs/{run}"))?),
        false => None,
    };
    // Live traces read the run database file, which a run in a store of the host lacks.
    let telemetry = if real && arguments.run_store.is_none() {
        setup
            .telemetry
            .as_ref()
            .and_then(|telemetry| telemetry.start(arguments, &mut recorder))
    } else {
        None
    };
    if let Some(listener) = &listener {
        let feed = events::live(listener, &run, &arguments.secrets);
        setup.live = Some(feed.map_err(|error| Failure::setup(format!("uwe: {error}")))?);
    }
    tell(Event::Started {
        run: run.clone(),
        time: events::milliseconds(std::time::SystemTime::now()),
    });
    // The inputs are checked before any task starts (decision 24).
    let prepared = executor(
        arguments,
        &setup,
        &run,
        verbose,
        !arguments.no_cache,
        pinned,
        &measured,
    )
    .and_then(|executor| {
        let mut driver = driver(
            language, &launch, &run, quiet, arguments, &setup, &interrupt,
        )?;
        let inputs = match admission {
            Some(policy) => {
                let inputs = match &source {
                    Some((launch, language)) => {
                        // It reads the inputs only, so the code of a Snakefile writes nothing.
                        let planning = Arguments {
                            planning: true,
                            ..arguments.clone()
                        };
                        let mut source = driver::driver(
                            *language, launch, &run, quiet, &planning, &setup, &interrupt,
                        )?;
                        source.inputs()?
                    }
                    None => driver.inputs()?,
                };
                let admission = application::validate_json(
                    Description {
                        language,
                        inputs,
                        outputs: None,
                    },
                    started.simulated,
                    policy,
                )
                .map_err(|error| failure(error.to_string()))?;
                if let Some(hosted) = &hosted {
                    hosted.authorize(&admission)?;
                }
                // A wir driver reads the values of a compiled run from its input file.
                if let Some(inputs) = admission.inputs.as_ref().filter(|_| source.is_none()) {
                    driver.use_inputs(inputs.values.clone())?;
                }
                admission.inputs
            }
            // The values of a compiled document come from its compile, as its language reads
            // them; a document that runs as it is checks them with its schema.
            None if source.is_some() => None,
            None => inputs::check(driver.as_mut(), started.simulated, quiet)?,
        };
        if let Some(inputs) = inputs {
            recorder.inputs(&inputs.schema, &inputs.values);
        }
        // A workflow whose outputs cannot be described still runs.
        if let Ok(Some(schema)) = driver.outputs() {
            recorder.outputs(&schema);
        }
        Ok((executor, driver))
    });
    let (executor, mut driver) = match prepared {
        Ok(prepared) => prepared,
        Err(failure) => {
            recorder.ended(Some(&failure.message));
            tell(events::ended(&run, Some(&failure.message)));
            close_checked(recorder, strict)?;
            return Err(failure);
        }
    };
    // A real run keeps the files of its workflow and the remote files it read in its store,
    // so that a repeat of the run has them years later.
    let kept = match real {
        true if strict => Some(setup.store(arguments)?),
        true => setup.store(arguments).ok(),
        false => None,
    };
    if let Some(store) = &kept
        && let Err(message) = runtime.block_on(sources::keep(store, &sources))
    {
        if strict {
            recorder.ended(Some(&message));
            tell(events::ended(&run, Some(&message)));
            close_checked(recorder, true)?;
            return Err(Failure::setup(format!(
                "workflow provenance was not retained: {message}"
            )));
        }
        tracing::warn!("uwe: the store does not keep the files of the workflow: {message}");
    }
    if let (true, Some(reason)) = (real, &sources.skipped) {
        tracing::info!("uwe: the run records no files of its workflow, as {reason}");
    }
    let taken = resume::Taken::default();
    let executor = match reusable {
        Some(reusable) => {
            let mode = if real {
                resume::Mode::Real
            } else {
                resume::Mode::Simulated
            };
            let resumed = resume::Resumed::new(
                executor,
                reusable,
                taken.clone(),
                mode,
                network_rule(arguments, &setup),
                verbose,
            );
            Box::new(resumed)
        }
        None => executor,
    };
    let time = arguments.task_seconds;
    let mut executor = backend::limited(executor, cpus, memory, time);
    // A real executor runs every task that can run, and the pool of its machine or cluster
    // sets how many run at once. The simulator runs one at a time, in the order asked.
    let options = RunOptions {
        max_pending: (arguments.max_pending).unwrap_or(if real { 1_024 } else { 1 }),
        order: (arguments.order).unwrap_or(if real {
            Order::Finished
        } else {
            Order::AsAsked
        }),
    };
    let progress = shown.then(|| {
        progress::Progress::start(progress::Header {
            target: workflow,
            language: written,
            executor: name,
            run: &run,
        })
    });
    let mut noting = resume::Noting::new(&mut recorder, taken);
    let mut watching;
    let observer: &mut dyn workflow_graph::Observer = match &progress {
        Some(progress) => {
            watching = progress.observer(&mut noting);
            &mut watching
        }
        None => &mut noting,
    };
    let mut terminal = record::Terminal {
        observer,
        defer: strict,
    };
    let mut forward;
    let observer: &mut dyn workflow_graph::Observer = match &listener {
        Some(listener) => {
            forward = events::Forward::new(listener, &run, &mut terminal);
            &mut forward
        }
        None => &mut terminal,
    };
    let mut execution = Engine::new(executor.as_mut())
        .options(options)
        .retries(arguments.machine_retries.unwrap_or(2))
        .retry_delay(std::time::Duration::from_millis(
            arguments.retry_delay_ms.unwrap_or(1_000),
        ))
        .observer(observer)
        .cancel(interrupt.clone())
        .no_cache_steps(arguments.no_cache_steps.clone())
        .run(&workflow.to_string_lossy(), driver.as_mut());
    // The graph is that of the workflow the user ran, whatever it compiled into.
    execution.graph.language = Some(written);
    if !real {
        execution.graph.simulate();
    }
    // The run is over: a later Ctrl-C, like the one that closes the page, stops nothing.
    interrupt.cancel();
    let read = fetched::read(&setup.storage, &store);
    recorder.remote_files(&fetched::rows(&read));
    if let Some(store) = &kept
        && let Err(message) = runtime.block_on(fetched::keep(store, &read))
    {
        if strict {
            recorder.ended(Some(&message));
            tell(events::ended(&run, Some(&message)));
            close_checked(recorder, true)?;
            return Err(Failure::setup(format!(
                "remote input provenance was not retained: {message}"
            )));
        }
        tracing::warn!("uwe: the store does not keep the remote files of the run: {message}");
    }
    if strict {
        recorder.finish(execution.error.as_ref());
    }
    if let Err(failure) = close_checked(recorder, strict) {
        tell(events::ended(&run, Some(&failure.message)));
        return Err(failure);
    }
    // A failed run leaves its error as the first line on standard error.
    if execution.error.is_none() && !arguments.no_right_sizing {
        for line in resources::notes(&execution.graph, &measured, !real) {
            eprintln!("{line}");
        }
    }
    // Traces of simulated runs would only fill dashboards with made-up times.
    if let Some(telemetry) = telemetry {
        telemetry.finish();
    }
    let files = executor.files();
    let report = report::after_run(arguments, &run, files.as_deref());
    match progress {
        Some(progress) => progress.finish(&run, Some(&store), report.as_deref()),
        None => {
            if let Some(report) = &report {
                eprintln!("uwe: the report of the run is in {}", report.display());
            }
        }
    }
    let error = execution.error.as_ref().map(ToString::to_string);
    tell(events::ended(&run, error.as_deref()));
    Ok(Ran {
        execution,
        page,
        id: run,
        outputs: driver.results(),
    })
}

/// A run that ended: what the engine did, the page that serves it, and its id in the run
/// database.
pub struct Run {
    pub execution: Execution,
    /// Optional local viewer requested in the settings; the host owns its lifetime.
    pub page: Option<Server>,
    pub id: String,
    /// The result values of a real run that finished, like the output object of CWL with the
    /// locations of its delivered files, what the entry workflow of Nextflow emits, or the
    /// outputs of a wir graph. `None` for simulated runs and other languages.
    pub outputs: Option<serde_json::Value>,
}

pub(crate) type Ran = Run;

/// Ends the record of a run. A run goes on when the database fails; it only says so.
pub(crate) fn close(recorder: Recorder) {
    let _ = close_checked(recorder, false);
}

fn close_checked(recorder: Recorder, strict: bool) -> Result<(), Failure> {
    if let Err(error) = recorder.close() {
        if strict {
            return Err(Failure::setup(format!(
                "the run database did not record the whole run: {error}"
            )));
        }
        tracing::warn!("uwe: the run database did not record the whole run: {error}");
    }
    Ok(())
}
