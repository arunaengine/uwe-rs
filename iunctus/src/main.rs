//! The `iunctus` command: `iunctus run` runs a workflow in any language iunctus knows,
//! `iunctus graph` prints the execution graph of a run, `iunctus config` prints the config of a
//! Nextflow project, `iunctus runs` lists the runs of the run database, `iunctus export`
//! writes the RO-Crate of a run, `iunctus page` serves the web page of the runs,
//! `iunctus why` tells why a task ran again, `iunctus origin` where a file came from,
//! `iunctus inputs` prints the JSON Schema of the inputs of a workflow, `iunctus outputs` that
//! of its outputs, `iunctus clean` removes from the store what no rule keeps,
//! `iunctus time` tells where the time of a run went, `iunctus compare` what differs
//! between two runs, `iunctus replay` runs one recorded task again, `iunctus telemetry`
//! writes and sends the OpenTelemetry trace of a run, `iunctus check-order` whether the
//! outputs of a workflow depend on the order in which its tasks finish, `iunctus changes`
//! what changed since a run, and `iunctus repeat` runs a past run again with what it pinned.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod arguments;
mod changes;
mod check;
mod clean;
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
mod record;
mod repeat;
mod resume;
mod revision;
mod runs;
mod setup;
mod slurm;
mod sources;
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
    let Some((command, rest)) = arguments.split_first() else {
        eprintln!("{USAGE}");
        return ExitCode::from(failure::USAGE);
    };
    let arguments = match arguments::parse(rest) {
        Ok(arguments) => arguments,
        Err(message) => return exit(Err(Failure::usage(format!("{message}; {HELP}")))),
    };
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
        "time" => insight::time::command(&arguments),
        "compare" => insight::compare::command(&arguments),
        "replay" => insight::replay::command(&arguments),
        "telemetry" => insight::telemetry::command(&arguments),
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
    let notes = simulation_notes(&execution.graph);
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
    if !graph.is_simulated() {
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
    let runtime =
        workflow_graph::runtime().map_err(|error| failure(format!("iunctus: {error}")))?;
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
    let record::Recording {
        mut recorder,
        reusable,
    } = record::start(arguments, &started)?;
    let page = match arguments.page {
        true => Some(page::start(arguments, &format!("/runs/{run}"))?),
        false => None,
    };
    // The inputs are checked before any task starts (decision 24).
    let prepared = executor(arguments, &setup, &run, !quiet, true, pinned).and_then(|executor| {
        let mut driver = driver(language, &launch, &run, quiet, arguments, &setup)?;
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
            let resumed = resume::Resumed::new(executor, reusable, taken.clone(), !quiet);
            Box::new(resumed)
        }
        None => executor,
    };
    // A real executor runs every task that can run, and the pool of its machine or cluster
    // sets how many run at once. The simulator runs one at a time, in the order asked.
    let options = RunOptions {
        max_pending: (arguments.max_pending).unwrap_or(if real { usize::MAX } else { 1 }),
        order: (arguments.order).unwrap_or(if real {
            Order::Finished
        } else {
            Order::AsAsked
        }),
    };
    let workflow = target.to_string_lossy();
    let mut observer = resume::Noting::new(&mut recorder, taken);
    let interrupt = stop_on_interrupt();
    let mut execution = Engine::new(executor.as_mut())
        .options(options)
        .observer(&mut observer)
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
    // Traces of simulated runs would only fill dashboards with made-up times.
    if real {
        insight::telemetry::after_run(arguments, &setup, &run);
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

/// A token that fires on Ctrl-C, so that the run stops its tasks cleanly. A second Ctrl-C ends
/// iunctus at once.
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
            if tokio::signal::ctrl_c().await.is_ok() && !stop.is_cancelled() {
                eprintln!("iunctus: stopping the running tasks; press Ctrl-C again to quit now");
                stop.cancel();
            }
            if tokio::signal::ctrl_c().await.is_ok() {
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
) -> Result<Box<dyn Driver>, DriverError> {
    match language {
        Language::Nextflow => nextflow::driver(launch, run, quiet, setup),
        Language::Snakemake => snakemake_runtime::driver_with(launch, snakemake(arguments, setup)),
        Language::Cwl => {
            let downloads = cache_directory().join("downloads");
            cwl_runtime::driver_with(launch, &setup.storage, downloads)
        }
        Language::Argo => argo_runtime::driver(launch),
    }
}

/// The settings of Snakemake runs: the Python container of decision 19, with the engine of
/// the local executor, which keeps what uv downloads and installs in the cache, and the
/// storage providers.
fn snakemake(arguments: &Arguments, setup: &Setup) -> snakemake_runtime::Settings {
    snakemake_runtime::Settings {
        python: Some(snakemake_runtime::PythonContainers {
            engine: setup.engine(),
            image: arguments.python_image.clone(),
            cache: cache_directory().join("python"),
            show_output: true,
        }),
        storage: setup.storage.clone(),
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
) -> Result<Box<dyn Executor>, Failure> {
    let cache = cache_directory();
    let scratch = cache.join("scratch");
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
            Ok(real(executor, arguments, verbose, reuse, pinned))
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
                scratch: &scratch,
            };
            let executor = tes::executor(store, place, run, default_image, storage)
                .map_err(|message| Failure::setup(format!("iunctus: {message}")))?;
            Ok(real(executor, arguments, verbose, reuse, pinned))
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
            let executor = slurm::executor(
                store,
                &scratch,
                run,
                default_image,
                storage,
                container,
                options,
            )
            .map_err(|message| Failure::setup(format!("iunctus: {message}")))?;
            Ok(real(executor, arguments, verbose, reuse, pinned))
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
            Ok(real(executor, arguments, verbose, reuse, pinned))
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
    let Some(target) = &arguments.target else {
        return Default::default();
    };
    let workflow = std::path::absolute(target).unwrap_or_else(|_| target.clone());
    let workflow = workflow.to_string_lossy();
    (runs::open(arguments).ok())
        .and_then(|database| database.chains(&workflow).ok())
        .unwrap_or_default()
}

/// The simulator with the preview of the cache: tasks whose results the store holds, as a real
/// run would find them, come from there (see [`preview`]). Without a store or a run database
/// to read, every task is simulated.
fn previewed(simulator: Simulator, arguments: &Arguments, setup: &Setup) -> Box<dyn Executor> {
    let pins = runs::open(arguments).and_then(|database| {
        (database.pins()).map_err(|error| failure(format!("iunctus: {error}")))
    });
    let (Ok(pins), Ok(store)) = (pins, setup.store(arguments)) else {
        return Box::new(simulator);
    };
    let settings = task_runner::Settings {
        downloads: cache_directory().join("downloads"),
        import_inputs: false,
        storage: setup.storage.clone(),
    };
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
/// answers tasks, the share of cached tasks that run again for the reproducibility check, and
/// the images that a repeat of a run pins.
fn real<B: executor::Backend>(
    mut executor: task_runner::RunnerExecutor<B>,
    arguments: &Arguments,
    verbose: bool,
    reuse: bool,
    pinned: &Pinned,
) -> Box<dyn Executor> {
    executor.verbose = verbose;
    executor.reuse = reuse;
    executor.check_reproducibility(arguments.check_reproducibility.unwrap_or(0.0));
    executor.pin_images(&pinned.images);
    Box::new(executor)
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
       iunctus inputs <workflow file or directory> [-format json|text] [-check] [options]
       iunctus outputs <workflow file or directory> [-format json|text] [options]
       iunctus clean [-keep-days <n>] [-keep-run <run>]... [-keep-slower-than <time>] [-remove]
       iunctus check-order <workflow file or directory> [options]
       iunctus changes <run>
       iunctus repeat <run> [-executor <executor>] [-store <folder or address>] [options]

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
                                      the simulator, no limit for real executors)
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
  -params-file <file>, -inputs <file> a params file, or a CWL job file
  -entry <name>                       the entry workflow or target
  -configfile <file>                  a Snakemake config file
  --config <name>=<value>...          Snakemake config values
  --<param> <value>                   a workflow parameter
  -resume [<run>]                     continues a run, by default the last one of the workflow,
                                      without running its finished tasks again
  -page                               serves the web page of the run while it goes on
  -address <ip>, -port <number>       where the page listens (default: 127.0.0.1, a free port)
  -database <file>                    the run database (default: runs.sqlite in the cache)
  -output <folder or file>            where `export` writes the RO-Crate (default: <run>-crate),
                                      and `telemetry` the trace (default:
                                      <run>-traces.jsonl)
  -license <url>                      the license of the exported crate
  -format json|text                   what `inputs` writes: the JSON Schema of the inputs
                                      (the default), or the inputs as help text;
                                      for `outputs` the same of the outputs
  -check                              `inputs` also checks the given values against the
                                      schema, as a run does before any task starts
  -keep-days <n>                      `clean` keeps what runs made or used in the last n days
  -keep-run <run>                     `clean` keeps what the run needs; give it once per run
  -keep-slower-than <time>            `clean` keeps the results of tasks that ran this long,
                                      like 90s, 10m, 2h or 1d
  -remove                             `clean` removes what it lists; without it, it only lists
                                      what it would remove
  -check-reproducibility <percent>    runs this share of the tasks whose results come from the
                                      cache again, like 10%, and compares their outputs; a tool
                                      that gives other bytes shares no results any more";

/// `$XDG_CACHE_HOME/iunctus`, else `~/.cache/iunctus`.
fn cache_directory() -> PathBuf {
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
