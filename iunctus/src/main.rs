//! The `iunctus` command: `iunctus run` runs a workflow in any language iunctus knows,
//! `iunctus graph` prints the execution graph of a run, `iunctus config` prints the config of a
//! Nextflow project, `iunctus runs` lists the runs of the run database, `iunctus export`
//! writes the RO-Crate of a run, `iunctus page` serves the web page of the runs,
//! `iunctus why` tells why a task ran again, `iunctus origin` where a file came from,
//! `iunctus inputs` prints the JSON Schema of the inputs of a workflow, `iunctus outputs` that
//! of its outputs, `iunctus clean` removes from the store what no rule keeps,
//! `iunctus time` tells where the time of a run went, `iunctus compare` what differs
//! between two runs, `iunctus replay` runs one recorded task again, `iunctus telemetry`
//! writes and sends the OpenTelemetry trace of a run, `iunctus report` writes the report of a
//! run as one HTML file, `iunctus check-order` whether the
//! outputs of a workflow depend on the order in which its tasks finish, `iunctus changes`
//! what changed since a run, and `iunctus repeat` runs a past run again with what it pinned.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod arguments;
mod changes;
mod check;
mod clean;
mod conda;
mod detect;
mod failure;
mod fetched;
mod inputs;
mod insight;
mod kubernetes;
mod local;
mod machine;
mod nextflow;
mod order;
mod page;
mod preview;
mod progress;
mod record;
mod repeat;
mod report;
mod resources;
mod resume;
mod revision;
mod runs;
mod setup;
mod slurm;
mod sources;
mod terminal;
mod tes;
mod tracked;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use run_database::Recorder;
use run_page::Server;
use workflow_graph::render::{Format, Options, render};
use workflow_graph::{
    CancellationToken, Driver, DriverError, Engine, Execution, ExecutionGraph, Executor, Language,
    Launch, Order, RunOptions, SIMULATION, Simulator,
};

use crate::arguments::Arguments;
use crate::failure::{Failure, INTERRUPTED, failure};
use crate::repeat::Pinned;
use crate::setup::Setup;

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments.as_slice() == ["__cwl-js-worker"] {
        return match cwl_runtime::javascript_worker_main() {
            Ok(()) => ExitCode::SUCCESS,
            Err(message) => {
                eprintln!("{message}");
                ExitCode::FAILURE
            }
        };
    }
    let Some((command, rest)) = arguments.split_first() else {
        eprintln!("{USAGE}");
        return ExitCode::from(failure::USAGE);
    };
    let mut arguments = match arguments::parse(rest) {
        Ok(arguments) => arguments,
        Err(message) => return exit(Err(Failure::usage(format!("{message}; {HELP}")))),
    };
    if !matches!(command.as_str(), "help" | "-help" | "--help") {
        match setup::Defaults::load(&arguments) {
            Ok(defaults) => defaults.apply(command, &mut arguments),
            Err(failure) => return exit(Err(failure)),
        }
    }
    let result = match command.as_str() {
        "run" => run(&arguments),
        "graph" => graph(&arguments),
        "check" => check::command(&arguments),
        "check-machine" => machine::command(&arguments),
        "config" => config(&arguments),
        "runs" => runs::list(&arguments),
        "export" => runs::export(&arguments),
        "page" => page::command(&arguments),
        "why" => runs::why(&arguments),
        "origin" => runs::origin(&arguments),
        "inputs" => inputs::command(&arguments),
        "outputs" => inputs::outputs(&arguments),
        "clean" => clean::command(&arguments),
        "check-order" => order::command(&arguments),
        "changes" => changes::command(&arguments),
        "repeat" => repeat::command(&arguments),
        "lock-environment" => conda::command(&arguments),
        "time" => insight::time::command(&arguments),
        "compare" => insight::compare::command(&arguments),
        "replay" => insight::replay::command(&arguments),
        "telemetry" => insight::telemetry::command(&arguments),
        "report" => report::command(&arguments),
        "help" | "-help" | "--help" => {
            println!("{USAGE}");
            Ok(())
        }
        _ => Err(Failure::usage(format!(
            "unknown command `{command}`; {HELP}"
        ))),
    };
    exit(result)
}

/// The exit code of a command, after its error, if any, on standard error.
fn exit(result: Result<(), Failure>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            for line in failure.lines() {
                eprintln!("{line}");
            }
            ExitCode::from(failure.code)
        }
    }
}

/// `iunctus run`: runs the workflow and writes its graph to `-graph` if given. With `-page`,
/// the page of the run stays open after the run until Ctrl-C.
fn run(arguments: &Arguments) -> Result<(), Failure> {
    if arguments.format.is_some() {
        return Err(Failure::usage(
            "iunctus: `-format` is an option of `iunctus graph`",
        ));
    }
    let Ran {
        execution, page, ..
    } = execute(arguments, false, &Pinned::default())?;
    if let Some(file) = &arguments.graph {
        write_graph(file, &execution.graph, &picture(arguments))?;
    }
    if let Some(page) = page {
        eprintln!("iunctus: the run has ended; the page stays open until you press Ctrl-C");
        page.serve_until_interrupted();
    }
    finish(execution)
}

/// `iunctus graph`: runs the workflow, with the simulator unless `-executor` says otherwise,
/// and prints its graph. What the workflow prints goes to standard error.
fn graph(arguments: &Arguments) -> Result<(), Failure> {
    if arguments.page {
        return Err(Failure::usage(
            "iunctus: `-page` is an option of `iunctus run`",
        ));
    }
    let options = picture(arguments);
    let format = match arguments.format.as_deref() {
        None => Format::default(),
        Some(name) => Format::parse(name).ok_or_else(|| {
            Failure::usage(format!(
                "iunctus: `-format {name}` is a format of `iunctus inputs`; use json, dot, \
                 summary or svg"
            ))
        })?,
    };
    let Ran { execution, .. } = execute(arguments, true, &Pinned::default())?;
    if let Some(file) = &arguments.graph {
        write_graph(file, &execution.graph, &options)?;
    }
    // A failed run still shows the graph so far.
    print!("{}", render(&execution.graph, &options, format));
    finish(execution)
}

/// Ends a command with its run: the error of a failed run comes first, so that it is the first
/// line on standard error, then the note of a simulated run.
fn finish(execution: Execution) -> Result<(), Failure> {
    let mut notes = simulation_notes(&execution.graph);
    if let Some(error) = &execution.failure_hook_error {
        notes.insert(
            0,
            format!("iunctus: error while ending the failed workflow: {error}"),
        );
    }
    match execution.into_result() {
        Ok(_) => {
            for note in notes {
                eprintln!("{note}");
            }
            Ok(())
        }
        Err(error) => {
            let mut failure = Failure::from(error);
            failure.after = notes;
            Err(failure)
        }
    }
}

/// What a simulated run says on standard error, which keeps standard output free for the
/// graph: that it is a simulation, and how many points of its graph are marked.
fn simulation_notes(graph: &ExecutionGraph) -> Vec<String> {
    // A run that asked for no task, like one that stopped while it loaded, simulated nothing.
    if !graph.is_simulated() || graph.nodes.is_empty() {
        return Vec::new();
    }
    let mut notes = vec![format!("iunctus: {SIMULATION}")];
    notes.extend(graph.cache_note().map(|note| format!("iunctus: {note}")));
    match graph.marked().count() {
        0 => {}
        1 => notes.push(
            "iunctus: the graph marks 1 point where real data can add or remove tasks.".into(),
        ),
        count => notes.push(format!(
            "iunctus: the graph marks {count} points where real data can add or remove tasks."
        )),
    }
    notes
}

/// Runs the target with its language's driver and the chosen executor, and records the run
/// in the run database. `quiet` keeps standard output free for the graph. With `-page`, the
/// page of the run serves from the start; the result holds it.
fn execute(arguments: &Arguments, quiet: bool, pinned: &Pinned) -> Result<Ran, Failure> {
    let (target, language) = target_and_language(arguments)?;
    let target = &target;
    let launch = launch(arguments, target.clone());
    let run = run_id();
    let name = arguments.executor.as_deref().unwrap_or("simulator");
    let setup = Setup::load(arguments)?;
    for file in &pinned.files {
        setup.storage.pin(file.clone());
    }
    let store = setup.store_location(arguments);
    let real = name != "simulator";
    // A real run shows its steps as it goes; in a terminal they are drawn in place, so the
    // executors print no line per task there.
    let shown = real && !quiet;
    let verbose = !quiet && !(shown && terminal::possible());
    let runtime =
        workflow_graph::runtime().map_err(|error| failure(format!("iunctus: {error}")))?;
    let interrupt = stop_on_interrupt();
    let _stop = interrupt.clone().drop_guard();
    let sources = runtime.block_on(sources::collect(target, arguments));
    let started = record::Started {
        id: &run,
        target,
        language,
        executor: name,
        simulated: !real,
        store: real.then_some(store.as_str()),
        sources: &sources,
        repeats: pinned.repeats.as_deref(),
    };
    // Read before this run is in the database.
    let measured = resources::measured(arguments);
    let record::Recording {
        mut recorder,
        reusable,
    } = record::start(arguments, &started)?;
    let page = match arguments.page {
        true => Some(page::start(arguments, &format!("/runs/{run}"))?),
        false => None,
    };
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
        if let Some(inputs) = inputs::check(driver.as_mut(), started.simulated, quiet)? {
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
            close(recorder);
            return Err(failure);
        }
    };
    // A real run keeps the files of its workflow and the remote files it read in its store,
    // so that a repeat of the run has them years later.
    let kept = match real {
        true => setup.store(arguments).ok(),
        false => None,
    };
    if let Some(store) = &kept
        && let Err(message) = runtime.block_on(sources::keep(store, &sources))
    {
        eprintln!("iunctus: warning: the store does not keep the files of the workflow: {message}");
    }
    if let (true, Some(reason)) = (real, &sources.skipped) {
        eprintln!("iunctus: note: the run records no files of its workflow, as {reason}");
    }
    let taken = resume::Taken::default();
    let mut executor = match reusable {
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
    let workflow = target.to_string_lossy();
    let progress = shown.then(|| {
        progress::Progress::start(progress::Header {
            target,
            language,
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
    let mut execution = Engine::new(executor.as_mut())
        .options(options)
        .observer(observer)
        .cancel(interrupt.clone())
        .run(&workflow, driver.as_mut());
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
        eprintln!(
            "iunctus: warning: the store does not keep the remote files of the run: {message}"
        );
    }
    close(recorder);
    // A failed run leaves its error as the first line on standard error.
    if execution.error.is_none() && !arguments.no_right_sizing {
        for line in resources::notes(&execution.graph, &measured, !real) {
            eprintln!("{line}");
        }
    }
    // Traces of simulated runs would only fill dashboards with made-up times.
    if real {
        insight::telemetry::after_run(arguments, &setup, &run);
    }
    let report = report::after_run(arguments, &run);
    match progress {
        Some(progress) => progress.finish(&run, Some(&store), report.as_deref()),
        None => {
            if let Some(report) = &report {
                eprintln!("iunctus: the report of the run is in {}", report.display());
            }
        }
    }
    Ok(Ran {
        execution,
        page,
        id: run,
    })
}

/// A run that ended: what the engine did, the page that serves it, and its id in the run
/// database.
struct Ran {
    execution: Execution,
    page: Option<Server>,
    id: String,
}

/// Ends the record of a run. A run goes on when the database fails; it only says so.
fn close(recorder: Recorder) {
    if let Err(error) = recorder.close() {
        eprintln!("iunctus: the run database did not record the whole run: {error}");
    }
}

/// A token that fires on Ctrl-C or SIGTERM, so that the run stops its tasks cleanly, as a
/// SLURM job or a service manager stops a program with SIGTERM. A second one ends iunctus at
/// once.
fn stop_on_interrupt() -> CancellationToken {
    let token = CancellationToken::new();
    let stop = token.clone();
    std::thread::spawn(move || {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return;
        };
        runtime.block_on(async {
            use tokio::signal::unix::{SignalKind, signal};
            let mut terminate = signal(SignalKind::terminate()).ok();
            let mut asked = async || {
                let terminated = async {
                    match terminate.as_mut() {
                        Some(terminate) => terminate.recv().await,
                        None => std::future::pending().await,
                    }
                };
                tokio::select! {
                    interrupted = tokio::signal::ctrl_c() => interrupted.is_ok(),
                    _ = terminated => true,
                }
            };
            if asked().await && !stop.is_cancelled() {
                eprintln!("iunctus: stopping the running tasks; press Ctrl-C again to quit now");
                stop.cancel();
            }
            if asked().await {
                std::process::exit(INTERRUPTED.into());
            }
        });
    });
    token
}

/// The workflow of a command and its language: `-lang`, else what the target tells.
fn target_and_language(arguments: &Arguments) -> Result<(PathBuf, Language), Failure> {
    let Some(target) = &arguments.target else {
        return Err(Failure::usage(
            "iunctus: name a workflow file or a project directory to run",
        ));
    };
    let language = match arguments.language {
        Some(language) => language,
        None => {
            detect::language(target).map_err(|message| failure(format!("iunctus: {message}")))?
        }
    };
    Ok((target.clone(), language))
}

/// The driver of a language.
fn driver(
    language: Language,
    launch: &Launch,
    run: &str,
    quiet: bool,
    arguments: &Arguments,
    setup: &Setup,
    cancel: &CancellationToken,
) -> Result<Box<dyn Driver>, DriverError> {
    match language {
        Language::Nextflow => nextflow::driver(launch, run, quiet, arguments, setup),
        Language::Snakemake => {
            snakemake_runtime::driver_with(launch, snakemake(arguments, setup, cancel))
        }
        Language::Cwl => {
            let downloads = cache_directory().join("downloads");
            let options = cwl_runtime::Options {
                javascript: cwl_runtime::JavaScriptSettings::new(setup.engine(), cancel.clone()),
                ..cwl_runtime::Options::default()
            };
            cwl_runtime::driver_with_options(launch, &setup.storage, downloads, options)
        }
        Language::Argo => argo_runtime::driver(launch),
    }
}

/// The settings of Snakemake runs: the Python container of decision 19, with the engine of
/// the local executor, which keeps what uv downloads and installs in the cache, and the
/// storage providers.
fn snakemake(
    arguments: &Arguments,
    setup: &Setup,
    cancel: &CancellationToken,
) -> snakemake_runtime::Settings {
    snakemake_runtime::Settings {
        python: Some(snakemake_runtime::PythonContainers {
            engine: setup.engine(),
            image: arguments.python_image.clone(),
            cache: cache_directory().join("python"),
            show_output: true,
        }),
        storage: setup.storage.clone(),
        cancel: cancel.clone(),
    }
}

/// `iunctus config`: the config of a Nextflow project, by default the launch directory.
fn config(arguments: &Arguments) -> Result<(), Failure> {
    let setup = Setup::load(arguments)?;
    let project = arguments.target.clone().unwrap_or_else(|| ".".into());
    nextflow::print_config(&launch(arguments, project), &setup.storage)?;
    Ok(())
}

/// The executor that `-executor` names: `simulator` (the default) writes a TES task document
/// for each task and makes up its outputs, `local` runs each task in a container, `tes` on a
/// TES server, `slurm` as a SLURM batch job and `kubernetes` as a Kubernetes Job. The simulator
/// keeps its files in the scratch directory of the cache. Without `reuse`, real executors run
/// every task whatever the cache holds, as a replay of one task does.
fn executor(
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
    match arguments.executor.as_deref().unwrap_or("simulator") {
        "simulator" if arguments.check_reproducibility.is_some() => Err(Failure::usage(
            "iunctus: the reproducibility check runs cached tasks again, so it needs a real \
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
                local::executor(store, &scratch, run, default_image, storage, &engine)
                    .map_err(|message| Failure::setup(format!("iunctus: {message}")))?;
            executor.priorities = std::sync::Arc::new(chains(arguments));
            real(executor, arguments, setup, verbose, reuse, pinned, measured)
        }
        "tes" => {
            let default_image = arguments.default_image.clone();
            let settings = &setup.executors;
            let server = (arguments.tes_server.as_deref())
                .or(settings.tes_server.as_deref())
                .unwrap_or(tes::DEFAULT_SERVER);
            let token = match std::env::var("IUNCTUS_TES_TOKEN") {
                Ok(token) if !token.is_empty() => Some(token),
                _ => (settings.tes_token.as_ref().map(|secret| secret.reveal()))
                    .transpose()
                    .map_err(|error| Failure::setup(format!("iunctus: {error}")))?,
            };
            let store = setup.store(arguments)?;
            let storage = setup.storage.clone();
            let place = tes::Place {
                server,
                token,
                sites: &settings.tes_sites,
                scratch: &scratch,
            };
            let executor = tes::executor(store, place, run, default_image, storage)
                .map_err(|message| Failure::setup(format!("iunctus: {message}")))?;
            real(executor, arguments, setup, verbose, reuse, pinned, measured)
        }
        "slurm" => {
            let default_image = arguments.default_image.clone();
            let settings = &setup.executors;
            let named =
                (arguments.slurm_container.as_deref()).or(settings.slurm_container.as_deref());
            let container = slurm::container(named)
                .map_err(|message| Failure::usage(format!("iunctus: {message}")))?;
            let options = match arguments.slurm_options.as_deref() {
                Some(options) => options.split_whitespace().map(str::to_owned).collect(),
                None => settings.slurm_options.clone().unwrap_or_default(),
            };
            let store = setup.store(arguments)?;
            let storage = setup.storage.clone();
            let backend = slurm::backend(&scratch, container, options, settings.slurm_pull);
            let executor = slurm::executor(store, backend, run, default_image, storage)
                .map_err(|message| Failure::setup(format!("iunctus: {message}")))?;
            real(executor, arguments, setup, verbose, reuse, pinned, measured)
        }
        "kubernetes" => {
            let default_image = arguments.default_image.clone();
            let settings = &setup.executors;
            let kubeconfig = (arguments.kubeconfig.as_deref()).or(settings.kubeconfig.as_deref());
            let namespace = (arguments.kubernetes_namespace.clone())
                .or_else(|| settings.kubernetes_namespace.clone());
            let store = setup.store(arguments)?;
            let storage = setup.storage.clone();
            let executor = kubernetes::executor(
                store,
                &scratch,
                run,
                default_image,
                storage,
                kubeconfig,
                namespace,
            )
            .map_err(|message| Failure::setup(format!("iunctus: {message}")))?;
            real(executor, arguments, setup, verbose, reuse, pinned, measured)
        }
        other => Err(Failure::usage(format!(
            "iunctus: unknown executor `{other}`; use `simulator`, `local`, `tes`, `slurm` or \
             `kubernetes`"
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
fn recorded_workflow(arguments: &Arguments) -> Option<String> {
    let target = arguments.target.as_ref()?;
    let workflow = std::path::absolute(target).unwrap_or_else(|_| target.clone());
    Some(workflow.to_string_lossy().into_owned())
}

/// The simulator with the preview of the cache: tasks whose results the store holds, as a real
/// run would find them, come from there (see [`preview`]). Without a store or a run database
/// to read, every task is simulated.
fn previewed(simulator: Simulator, arguments: &Arguments, setup: &Setup) -> Box<dyn Executor> {
    // A real run without the cache would run every task.
    if arguments.no_cache {
        return Box::new(simulator);
    }
    let pins = runs::open(arguments).and_then(|database| {
        (database.pins()).map_err(|error| failure(format!("iunctus: {error}")))
    });
    let (Ok(pins), Ok(store)) = (pins, setup.store(arguments)) else {
        return Box::new(simulator);
    };
    let settings = runner_settings(setup.storage.clone(), false);
    let lookup = task_runner::Lookup::new(store, settings);
    let default_image = arguments.default_image.clone();
    Box::new(preview::Previewed::new(
        simulator,
        lookup,
        pins,
        default_image,
    ))
}

/// A real executor with the settings of the command line: its line per task, whether the cache
/// answers tasks, the share of cached tasks that run again for the reproducibility check, the
/// images that a repeat of a run pins, and which tasks get the network.
fn real<B: executor::Backend>(
    mut executor: task_runner::RunnerExecutor<B>,
    arguments: &Arguments,
    setup: &Setup,
    verbose: bool,
    reuse: bool,
    pinned: &Pinned,
    measured: &resources::Measures,
) -> Result<Box<dyn Executor>, Failure> {
    executor.verbose = verbose;
    executor.measured = measured.clone();
    executor.right_sizing = !arguments.no_right_sizing;
    executor.together = !arguments.no_grouping;
    executor.reuse = reuse;
    executor.check_reproducibility(arguments.check_reproducibility.unwrap_or(0.0));
    executor.pin_images(&pinned.images);
    executor.network = network_rule(arguments, setup);
    Ok(Box::new(executor))
}

/// Which tasks get the network: `[network]` of the config file, with `-no-network` for the
/// tasks whose workflow does not say.
fn network_rule(arguments: &Arguments, setup: &Setup) -> task_runner::NetworkRule {
    task_runner::NetworkRule {
        none_by_default: arguments.no_network || setup.network.none_by_default,
        ..setup.network.clone()
    }
}

/// An error when the run asks for tasks without network and its executor cannot cut a task
/// off the network yet: TES, Kubernetes, and SLURM with Singularity. It comes
/// before the executor starts, so that it is the first thing the user reads.
fn offline_possible(arguments: &Arguments, setup: &Setup) -> Result<(), Failure> {
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
        Some("slurm") if named == "singularity" => {
            "Singularity cuts a task off the network only where the site allows it, which \
             iunctus does not ask for (`-slurm-container apptainer`, `podman` or `docker` can)"
        }
        _ => return Ok(()),
    };
    Err(Failure::usage(format!(
        "iunctus: {why}, so tasks cannot run without network there yet; leave out \
         `-no-network` and `default = \"none\"` of `[network]`, or use `-executor local`"
    )))
}

/// What the command line asks to run, in the terms all languages share.
fn launch(arguments: &Arguments, target: PathBuf) -> Launch {
    Launch {
        target,
        params: arguments.params.clone(),
        inputs: arguments.inputs.clone(),
        profiles: arguments.profiles.clone().unwrap_or_default(),
        configs: arguments.configs.clone(),
        targets: arguments.targets.clone(),
        scratch: cache_directory().join("scratch"),
        output_dir: arguments.output_dir.clone(),
        stub: arguments.stub,
        // The simulator makes up the outputs of tasks, so its runs also assume that the
        // source files of the workflow exist, as the graph needs them rather than their content.
        simulated: arguments.executor.as_deref().unwrap_or("simulator") == "simulator",
        keep_going: arguments.keep_going,
    }
}

/// How to draw the graph: `-view`, `-direction` and `-fold`.
fn picture(arguments: &Arguments) -> Options {
    let defaults = Options::default();
    Options {
        view: arguments.view.unwrap_or(defaults.view),
        direction: arguments.direction.unwrap_or(defaults.direction),
        fold: arguments.fold.unwrap_or(defaults.fold),
        ..defaults
    }
}

/// Writes the graph in the format that the file name asks for: DOT for `.dot`, SVG for
/// `.svg`, a summary for `.txt`, else JSON.
fn write_graph(file: &Path, graph: &ExecutionGraph, options: &Options) -> Result<(), Failure> {
    let text = render(graph, options, Format::of_file(file));
    std::fs::write(file, text)
        .map_err(|error| failure(format!("iunctus: cannot write {}: {error}", file.display())))
}

/// What an error of the command line points to.
const HELP: &str = "`iunctus help` lists the commands and their options";

const USAGE: &str = "\
usage: iunctus run <workflow file or directory> [<targets>] [options]
       iunctus graph <workflow file or directory> [-format json|dot|summary|svg] [-view tasks|steps]
                     [-direction down|right] [-fold <n>] [options]
       iunctus check <workflow file or directory> [-default-image <image>] [options]
       iunctus check-machine [-executor <executor>] [-store <folder or address>]
       iunctus config [<project>] [-profile <names>] [-c <config>] [--<param> <value>]
       iunctus runs
       iunctus page [<run>] [-address <ip>] [-port <number>]
       iunctus export <run> [-output <folder>] [-license <url>]
       iunctus why <run> <task>
       iunctus origin <file>
       iunctus time <run> [-format text|json]
       iunctus compare <run> [<run>] [-format text|json]
       iunctus replay <run> <task> [-executor local|tes|slurm|kubernetes] [-store <place>]
       iunctus telemetry <run> [-output <file>]
       iunctus report <run> [-output <file>]
       iunctus inputs <workflow file or directory> [-format json|text] [-check] [options]
       iunctus outputs <workflow file or directory> [-format json|text] [options]
       iunctus clean [-keep-days <n>] [-keep-run <run>]... [-keep-slower-than <time>] [-remove]
       iunctus check-order <workflow file or directory> [options]
       iunctus changes <run>
       iunctus repeat <run> [-executor <executor>] [-store <folder or address>] [options]
       iunctus lock-environment <conda environment file> [-platform <name>] [-output <file>]

options:
  -lang nextflow|snakemake|cwl|argo   the language, when the target does not tell it
  -executor <executor>                simulator (the default) writes TES task documents
                                      and makes up the outputs; local runs containers on
                                      this machine, tes on a TES server, slurm as SLURM
                                      batch jobs, kubernetes as Kubernetes Jobs
  -graph <file>                       writes the execution graph: DOT for .dot, SVG for .svg,
                                      a summary for .txt, else JSON
  -view tasks|steps                   one node per task (the default) or one per step
  -direction down|right               which way DOT and SVG pictures of the graph go
                                      (default: down)
  -fold <n>                           in pictures of the graph of tasks, a step with more
                                      than n tasks is one box (default 5; 0 draws every task)
  -output-dir <folder>                where CWL outputs and Argo artifacts go
  -tes-server <address>               the TES server of -executor tes; Funnel's
                                      http://localhost:8000 by default
  -slurm-container <runtime>          how the nodes of -executor slurm run containers:
                                      apptainer (the default), singularity, podman or docker
  -slurm-options '<options>'          more options of sbatch, like '--account=lab'
  -kubeconfig <file>                  the kubeconfig of the cluster of -executor kubernetes;
                                      iunctus never takes the default one
  -kubernetes-namespace <name>        the namespace of the Jobs; the kubeconfig's by default
  -max-pending <n>                    lets up to n tasks be pending at once (default 1 for
                                      the simulator, 1024 for real executors)
  -order asked|reversed|finished|shuffled[:<seed>]
                                      the order in which pending tasks finish (default
                                      asked for the simulator, finished for real executors)
  -store <folder or address>          the cache store of real executors, in a folder or at an
                                      address of a storage provider, like s3://bucket/cache
  -iunctus-config <file>              a config file of iunctus, read after
                                      /etc/iunctus/config.toml and
                                      ~/.config/iunctus/config.toml
  -default-image <image>              the image of tasks that name none, for real executors
  -python-image <image>               the image of the Python packages without a stand-in
  -stub                               runs the stubs of tasks
  -keep-going                         after a task failed, runs every task that does not
                                      depend on it, then fails; without it no more tasks
                                      start, as each language's engine does
  -profile <names>, -c <file>         Nextflow profiles and config files
  -params-file <file>, -inputs <file> a params file, or a CWL job file; as with cwltool,
                                      `run tool.cwl job.yml` takes job.yml as the job file
  -entry <name>                       the entry workflow or target
  -configfile <file>                  a Snakemake config file
  --config <name>=<value>...          Snakemake config values
  --<param> <value>                   a workflow parameter
  -resume [<run>]                     continues a run, by default the last one of the workflow,
                                      without running its finished tasks again
  -no-cache                           takes no result from the cache and keeps none there:
                                      every task runs
  -no-right-sizing                    tasks ask for the CPUs and memory that the workflow
                                      names, not for what earlier runs of their step used
  -no-grouping                        runs each task in a container of its own; without it,
                                      short tasks of a step run one after another in one
                                      container
  -page                               serves the web page of the run while it goes on
  -report <file>                      writes the report of the run when it ends: one HTML
                                      file with the page of the run, to open without a server
  -address <ip>, -port <number>       where the page listens (default: 127.0.0.1, a free port)
  -database <file>                    the run database (default: runs.sqlite in the cache)
  -output <folder or file>            where `export` writes the RO-Crate (default: <run>-crate),
                                      `telemetry` the trace (default: <run>-traces.jsonl),
                                      `report` the report (default: <run>-report.html),
                                      and `lock-environment` the lock (default: standard
                                      output; a .txt file gets conda's explicit list)
  -license <url>                      the license of the exported crate
  -platform <name>                    the conda platform of `lock-environment`, like linux-64
                                      (default: Linux on the processor of this machine)
  -format json|text|form              what `inputs` writes: JSON Schema, help text, or a JSON form description;
                                      for `outputs` use json (the default) or text
  -check                              `inputs` also checks the given values against the
                                      schema, as a run does before any task starts
  -keep-days <n>                      `clean` keeps what runs made or used in the last n days
  -keep-run <run>                     `clean` keeps what the run needs; give it once per run
  -keep-slower-than <time>            `clean` keeps the results of tasks that ran this long,
                                      like 90s, 10m, 2h or 1d
  -remove                             `clean` removes what it lists; without it, it only lists
                                      what it would remove
  -no-network                         runs the tasks without network whose workflow does not
                                      say that they need it; `[network] allowed_steps` of the
                                      config file names steps that get it anyway
  -check-reproducibility <percent>    runs this share of the tasks whose results come from the
                                      cache again, like 10%, and compares their outputs; a tool
                                      that gives other bytes shares no results any more";

/// How the task runner treats the inputs of tasks: remote files are downloaded once into the
/// cache folder, and the digests of local files are kept there, so that a file that did not
/// change is not read again. `import_inputs` copies local inputs into the store too, for
/// executors whose tasks run on other machines.
fn runner_settings(storage: storage::Storage, import_inputs: bool) -> task_runner::Settings {
    let cache = cache_directory();
    let digests = cache.join(task_runner::DigestCache::FILE_NAME);
    task_runner::Settings {
        downloads: cache.join("downloads"),
        import_inputs,
        storage,
        // Without the cache of digests, inputs are read as before; it only saves time.
        digests: task_runner::DigestCache::open(&digests).ok(),
    }
}

/// The cache folder that `[cache] folder` of the config file names, set once at the start.
static CACHE_FOLDER: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Uses this cache folder for the whole command, in place of the one of `XDG_CACHE_HOME`.
fn use_cache_folder(folder: PathBuf) {
    let _ = CACHE_FOLDER.set(folder);
}

/// The folder of `[cache] folder` of the config file, else `$XDG_CACHE_HOME/iunctus`, else
/// `~/.cache/iunctus`.
fn cache_directory() -> PathBuf {
    if let Some(folder) = CACHE_FOLDER.get() {
        return folder.clone();
    }
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("iunctus")
}

/// A run id from the clock and the process id.
fn run_id() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default();
    format!("{now:x}-{:x}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "needs Docker and the cached debian:stable-slim image"]
    fn restricted_argo_host_inputs_are_readable_as_the_image_user() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("private.txt");
        std::fs::write(&source, "private input\n").unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o600)).unwrap();
        for uid in [0, 65534] {
            let target = root.path().join("workflow.yaml");
            std::fs::write(&target, format!(
                "apiVersion: argoproj.io/v1alpha1\nkind: Workflow\nmetadata: {{name: read}}\nspec:\n\
                 \x20 entrypoint: read\n  volumes:\n  - name: source\n    hostPath: {{path: '{}', type: File}}\n\
                 \x20 templates:\n  - name: read\n    container:\n      image: debian:stable-slim\n\
                 \x20     command: [sh, -c]\n      args: ['cat /data/private.txt > /tmp/read.txt']\n\
                 \x20     securityContext: {{runAsUser: {uid}, runAsGroup: {uid}}}\n\
                 \x20     volumeMounts: [{{name: source, mountPath: /data/private.txt, readOnly: true}}]\n\
                 \x20   outputs:\n      parameters: [{{name: value, valueFrom: {{path: /tmp/read.txt}}}}]\n",
                source.display(),
            )).unwrap();
            let run = format!("permissions-{uid}");
            let store = content_store::Store::local(&root.path().join("store")).unwrap();
            let runtime = workflow_graph::runtime().unwrap();
            let engine = runtime
                .block_on(executor::ContainerEngine::named(Some("docker")))
                .unwrap();
            let backend = executor::LocalContainer::new(
                engine,
                root.path().join("scratch"),
                executor::Pool::new(2, 1 << 30),
            );
            let settings = task_runner::Settings {
                downloads: root.path().join("downloads"),
                import_inputs: false,
                storage: storage::Storage::default(),
                digests: None,
            };
            let mut executor = task_runner::RunnerExecutor::new(
                task_runner::Runner::new(store, backend, &run, settings),
                None,
            );
            let launch = Launch {
                target,
                scratch: root.path().join("planning"),
                ..Launch::default()
            };
            let mut driver = argo_runtime::driver(&launch).unwrap();
            let execution = Engine::new(&mut executor).run("permissions", driver.as_mut());
            assert!(
                execution.error.is_none(),
                "uid {uid}: {:?}",
                execution.error
            );
            assert_eq!(execution.graph.nodes.len(), 1);
            assert_eq!(execution.graph.nodes[0].exit_code, Some(0));
            assert_eq!(std::fs::read_to_string(&source).unwrap(), "private input\n");
            assert_eq!(
                std::fs::metadata(&source).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn failure_hook_diagnostic_preserves_the_original_exit_code() {
        let execution = Execution {
            graph: ExecutionGraph::default(),
            error: Some(workflow_graph::ExecuteError::Machine {
                task: "task".into(),
                message: "original machine failure".into(),
            }),
            failure_hook_error: Some(DriverError::unsupported("failure hook problem")),
        };
        let failure = finish(execution).expect_err("failed execution");
        assert_eq!(failure.code, failure::SETUP);
        assert!(failure.message.contains("original machine failure"));
        assert!(
            failure
                .after
                .iter()
                .any(|note| note.contains("failure hook problem"))
        );
    }
}
