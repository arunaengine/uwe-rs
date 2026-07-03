//! The `iunctus` command: `iunctus run` runs a workflow in any language iunctus knows,
//! `iunctus graph` prints the execution graph of a run, `iunctus config` prints the config of a
//! Nextflow project, `iunctus runs` lists the runs of the run database, `iunctus export`
//! writes the RO-Crate of a run, `iunctus page` serves the web page of the runs, and
//! `iunctus why` tells why a task ran again, and `iunctus origin` where a file came from.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod arguments;
mod detect;
mod local;
mod nextflow;
mod page;
mod record;
mod remote;
mod resume;
mod revision;
mod runs;
mod tes;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use run_database::Recorder;
use run_page::Server;
use workflow_graph::render::{Format, View, render};
use workflow_graph::{
    CancellationToken, Driver, DriverError, Engine, ExecuteError, Execution, ExecutionGraph,
    Executor, Language, Launch, Order, RunOptions, SIMULATION, Simulator,
};

use crate::arguments::Arguments;

/// The exit code for a feature that a workflow language supports but iunctus does not yet, as
/// CWL runners use it for unsupported requirements.
const UNSUPPORTED: u8 = 33;

/// The exit code of a run stopped with Ctrl-C, as shells give it.
const INTERRUPTED: u8 = 130;

/// Why a command failed, and its exit code.
struct Failure {
    message: String,
    code: u8,
    /// Lines for standard error after the message, like the note of a simulated run.
    after: Vec<String>,
}

impl From<DriverError> for Failure {
    fn from(error: DriverError) -> Self {
        Failure {
            code: if error.unsupported { UNSUPPORTED } else { 1 },
            message: error.message,
            after: Vec::new(),
        }
    }
}

impl From<ExecuteError> for Failure {
    fn from(error: ExecuteError) -> Self {
        let code = match &error {
            ExecuteError::Cancelled => INTERRUPTED,
            error if error.unsupported() => UNSUPPORTED,
            _ => 1,
        };
        Failure {
            code,
            message: error.to_string(),
            after: Vec::new(),
        }
    }
}

fn failure(message: impl Into<String>) -> Failure {
    Failure {
        message: message.into(),
        code: 1,
        after: Vec::new(),
    }
}

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let Some((command, rest)) = arguments.split_first() else {
        return usage();
    };
    let arguments = match arguments::parse(rest) {
        Ok(arguments) => arguments,
        Err(message) => {
            eprintln!("iunctus: {message}");
            return usage();
        }
    };
    let result = match command.as_str() {
        "run" => run(&arguments),
        "graph" => graph(&arguments),
        "config" => config(&arguments),
        "runs" => runs::list(&arguments),
        "export" => runs::export(&arguments),
        "page" => page::command(&arguments),
        "why" => runs::why(&arguments),
        "origin" => runs::origin(&arguments),
        _ => return usage(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            eprintln!("{}", failure.message);
            for line in &failure.after {
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
        return Err(failure(
            "iunctus: `-format` is an option of `iunctus graph`",
        ));
    }
    let (execution, page) = execute(arguments, false)?;
    if let Some(file) = &arguments.graph {
        write_graph(file, &execution.graph, arguments.view.unwrap_or_default())?;
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
        return Err(failure("iunctus: `-page` is an option of `iunctus run`"));
    }
    let view = arguments.view.unwrap_or_default();
    let (execution, _) = execute(arguments, true)?;
    if let Some(file) = &arguments.graph {
        write_graph(file, &execution.graph, view)?;
    }
    // A failed run still shows the graph so far.
    let format = arguments.format.unwrap_or_default();
    print!("{}", render(&execution.graph, view, format));
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
fn execute(arguments: &Arguments, quiet: bool) -> Result<(Execution, Option<Server>), Failure> {
    let Some(target) = &arguments.target else {
        return Err(failure(
            "iunctus: name a workflow file or a project directory to run",
        ));
    };
    let language = match arguments.language {
        Some(language) => language,
        None => {
            detect::language(target).map_err(|message| failure(format!("iunctus: {message}")))?
        }
    };
    let launch = launch(arguments, target.clone());
    let run = run_id();
    let name = arguments.executor.as_deref().unwrap_or("simulator");
    let store = store(arguments);
    let started = record::Started {
        id: &run,
        target,
        language,
        executor: name,
        simulated: name == "simulator",
        store: (name == "local").then_some(store.as_path()),
    };
    let record::Recording {
        mut recorder,
        reusable,
    } = record::start(arguments, &started)?;
    let page = match arguments.page {
        true => Some(page::start(arguments, &format!("/runs/{run}"))?),
        false => None,
    };
    let prepared = executor(arguments, &run, !quiet).and_then(|executor| {
        let driver = driver(language, &launch, &run, quiet, arguments)?;
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
    let real = name != "simulator";
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
    close(recorder);
    Ok((execution, page))
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

/// The driver of a language.
fn driver(
    language: Language,
    launch: &Launch,
    run: &str,
    quiet: bool,
    arguments: &Arguments,
) -> Result<Box<dyn Driver>, DriverError> {
    match language {
        Language::Nextflow => nextflow::driver(launch, run, quiet),
        Language::Snakemake => snakemake_runtime::driver_with(launch, snakemake(arguments)),
        Language::Cwl => cwl_runtime::driver(launch),
        Language::Argo => argo_runtime::driver(launch),
    }
}

/// The settings of Snakemake runs: the Python container of decision 19, with the engine of
/// the local executor, which keeps what uv downloads and installs in the cache.
fn snakemake(arguments: &Arguments) -> snakemake_runtime::Settings {
    snakemake_runtime::Settings {
        python: Some(snakemake_runtime::PythonContainers {
            engine: engine(),
            image: arguments.python_image.clone(),
            cache: cache_directory().join("python"),
            show_output: true,
        }),
    }
}

/// `iunctus config`: the config of a Nextflow project, by default the launch directory.
fn config(arguments: &Arguments) -> Result<(), Failure> {
    let project = arguments.target.clone().unwrap_or_else(|| ".".into());
    nextflow::print_config(&launch(arguments, project))?;
    Ok(())
}

/// The executor that `-executor` names: `simulator` (the default) writes a TES task document
/// for each task and makes up its outputs, `local` runs each task in a container. The
/// simulator keeps its files in the scratch directory of the cache.
fn executor(arguments: &Arguments, run: &str, verbose: bool) -> Result<Box<dyn Executor>, Failure> {
    let cache = cache_directory();
    let scratch = cache.join("scratch");
    match arguments.executor.as_deref().unwrap_or("simulator") {
        "simulator" => {
            let mut executor = Simulator::new(scratch.join("simulator").join(run));
            executor.verbose = verbose;
            Ok(Box::new(executor))
        }
        "local" => {
            let default_image = arguments.default_image.clone();
            let mut executor = local::executor(&store(arguments), &scratch, run, default_image)
                .map_err(|message| failure(format!("iunctus: {message}")))?;
            executor.verbose = verbose;
            Ok(Box::new(executor))
        }
        "tes" => {
            let default_image = arguments.default_image.clone();
            let server = (arguments.tes_server.as_deref()).unwrap_or(tes::DEFAULT_SERVER);
            let mut executor =
                tes::executor(&store(arguments), &scratch, run, default_image, server)
                    .map_err(|message| failure(format!("iunctus: {message}")))?;
            executor.verbose = verbose;
            Ok(Box::new(executor))
        }
        other => Err(failure(format!(
            "iunctus: unknown executor `{other}`; use `simulator`, `local` or `tes`"
        ))),
    }
}

/// The store of the local executor: `-store`, else `store` in the cache directory.
fn store(arguments: &Arguments) -> PathBuf {
    (arguments.store.clone()).unwrap_or_else(|| cache_directory().join("store"))
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
    }
}

/// Writes the graph in the format that the file name asks for: DOT for `.dot`, SVG for
/// `.svg`, a summary for `.txt`, else JSON.
fn write_graph(file: &Path, graph: &ExecutionGraph, view: View) -> Result<(), Failure> {
    let text = render(graph, view, Format::of_file(file));
    std::fs::write(file, text)
        .map_err(|error| failure(format!("iunctus: cannot write {}: {error}", file.display())))
}

fn usage() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(2)
}

const USAGE: &str = "\
usage: iunctus run <workflow file or directory> [<targets>] [options]
       iunctus graph <workflow file or directory> [-format json|dot|summary|svg] [-view tasks|steps] [options]
       iunctus config [<project>] [-profile <names>] [-c <config>] [--<param> <value>]
       iunctus runs
       iunctus page [<run>] [-address <ip>] [-port <number>]
       iunctus export <run> [-output <folder>] [-license <url>]
       iunctus why <run> <task>
       iunctus origin <file>

options:
  -lang nextflow|snakemake|cwl|argo   the language, when the target does not tell it
  -executor simulator|local|tes       simulator (the default) writes TES task documents
                                      and makes up the outputs; local runs containers on
                                      this machine, tes on a TES server
  -graph <file>                       writes the execution graph: DOT for .dot, SVG for .svg,
                                      a summary for .txt, else JSON
  -view tasks|steps                   one node per task (the default) or one per step
  -output-dir <folder>                where CWL outputs and Argo artifacts go
  -tes-server <address>               the TES server of -executor tes; Funnel's
                                      http://localhost:8000 by default
  -max-pending <n>                    lets up to n tasks be pending at once (default 1 for
                                      the simulator, no limit for real executors)
  -order asked|reversed|finished|shuffled[:<seed>]
                                      the order in which pending tasks finish (default
                                      asked for the simulator, finished for real executors)
  -store <directory>                  the cache of the local executor
  -default-image <image>              the image of tasks that name none, for real executors
  -python-image <image>               the image of the Python packages without a stand-in
  -stub                               runs the stubs of tasks
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
  -output <folder>                    where `export` writes the RO-Crate (default: <run>-crate)
  -license <url>                      the license of the exported crate";

/// `$XDG_CACHE_HOME/iunctus`, else `~/.cache/iunctus`.
fn cache_directory() -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("iunctus")
}

/// The container engine of the local executor: `$IUNCTUS_ENGINE`, else `docker`.
fn engine() -> String {
    std::env::var("IUNCTUS_ENGINE").unwrap_or_else(|_| "docker".into())
}

/// A run id from the clock and the process id.
fn run_id() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default();
    format!("{now:x}-{:x}", std::process::id())
}
