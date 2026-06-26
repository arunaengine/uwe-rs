//! The `iunctus` command: `iunctus run` runs a workflow in any language iunctus knows,
//! `iunctus graph` prints the execution graph of a run, `iunctus config` prints the config of a
//! Nextflow project, `iunctus runs` lists the runs of the run database, and `iunctus export`
//! writes the RO-Crate of a run.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod arguments;
mod checked;
mod detect;
mod local;
mod nextflow;
mod record;
mod remote;
mod resume;
mod revision;
mod runs;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use run_database::Recorder;
use workflow_graph::{
    Driver, DriverError, Engine, ExecuteError, Execution, ExecutionGraph, Executor, Language,
    Launch, RunOptions, Simulator,
};

use crate::arguments::Arguments;

/// The exit code for a feature that a workflow language supports but iunctus does not yet, as
/// CWL runners use it for unsupported requirements.
const UNSUPPORTED: u8 = 33;

/// Why a command failed, and whether the cause is a feature iunctus does not support yet.
struct Failure {
    message: String,
    unsupported: bool,
}

impl From<DriverError> for Failure {
    fn from(error: DriverError) -> Self {
        Failure {
            message: error.message,
            unsupported: error.unsupported,
        }
    }
}

impl From<ExecuteError> for Failure {
    fn from(error: ExecuteError) -> Self {
        Failure {
            unsupported: error.unsupported(),
            message: error.to_string(),
        }
    }
}

fn failure(message: impl Into<String>) -> Failure {
    Failure {
        message: message.into(),
        unsupported: false,
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
        _ => return usage(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            eprintln!("{}", failure.message);
            ExitCode::from(if failure.unsupported { UNSUPPORTED } else { 1 })
        }
    }
}

/// `iunctus run`: runs the workflow and writes its graph to `-graph` if given.
fn run(arguments: &Arguments) -> Result<(), Failure> {
    if arguments.format.is_some() {
        return Err(failure(
            "iunctus: `-format` is an option of `iunctus graph`",
        ));
    }
    let execution = execute(arguments, false)?;
    if let Some(file) = &arguments.graph {
        write_graph(file, &execution.graph)?;
    }
    execution.into_result()?;
    Ok(())
}

/// `iunctus graph`: runs the workflow, with the simulator unless `-executor` says otherwise,
/// and prints its graph. What the workflow prints goes to standard error.
fn graph(arguments: &Arguments) -> Result<(), Failure> {
    let format = arguments.format.as_deref().unwrap_or("summary");
    if !["json", "dot", "summary"].contains(&format) {
        return Err(failure(format!(
            "iunctus: unknown graph format `{format}`; use json, dot or summary"
        )));
    }
    let execution = execute(arguments, true)?;
    if let Some(file) = &arguments.graph {
        write_graph(file, &execution.graph)?;
    }
    // A failed run still shows the graph so far.
    print!("{}", render(&execution.graph, format));
    execution.into_result()?;
    Ok(())
}

/// Runs the target with its language's driver and the chosen executor, and records the run
/// in the run database. `quiet` keeps standard output free for the graph.
fn execute(arguments: &Arguments, quiet: bool) -> Result<Execution, Failure> {
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
    let options = RunOptions {
        max_pending: arguments.max_pending.unwrap_or(1),
        order: arguments.order.unwrap_or_default(),
    };
    let workflow = target.to_string_lossy();
    let mut observer = resume::Noting::new(&mut recorder, taken);
    let execution = Engine::new(executor.as_mut())
        .options(options)
        .observer(&mut observer)
        .run(&workflow, driver.as_mut());
    close(recorder);
    Ok(execution)
}

/// Ends the record of a run. A run goes on when the database fails; it only says so.
fn close(recorder: Recorder) {
    if let Err(error) = recorder.close() {
        eprintln!("iunctus: the run database did not record the whole run: {error}");
    }
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
/// simulator keeps its files in the scratch directory of the cache. A real executor gets its
/// tasks through the checks of [`checked::Checked`].
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
            let mut executor = local::Local::new(&store(arguments), &scratch, run)
                .map_err(|message| failure(format!("iunctus: {message}")))?;
            executor.verbose = verbose;
            let default_image = arguments.default_image.clone();
            Ok(Box::new(checked::Checked::new(executor, default_image)))
        }
        other => Err(failure(format!(
            "iunctus: unknown executor `{other}`; use `simulator` or `local`"
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
        stub: arguments.stub,
        // A simulated run makes up the outputs of tasks, so it also assumes that the source
        // files of the workflow exist, as the graph needs them rather than their content.
        assume_inputs: arguments.executor.as_deref() != Some("local"),
    }
}

/// Writes the graph as DOT when the file name ends with `.dot`, else as JSON.
fn write_graph(file: &Path, graph: &ExecutionGraph) -> Result<(), Failure> {
    let dot = file.extension().is_some_and(|extension| extension == "dot");
    let text = render(graph, if dot { "dot" } else { "json" });
    std::fs::write(file, text)
        .map_err(|error| failure(format!("iunctus: cannot write {}: {error}", file.display())))
}

fn render(graph: &ExecutionGraph, format: &str) -> String {
    match format {
        "dot" => graph.to_dot(),
        "summary" => graph.summary(),
        _ => serde_json::to_string_pretty(graph).unwrap_or_default() + "\n",
    }
}

fn usage() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(2)
}

const USAGE: &str = "\
usage: iunctus run <workflow file or directory> [<targets>] [options]
       iunctus graph <workflow file or directory> [-format json|dot|summary] [options]
       iunctus config [<project>] [-profile <names>] [-c <config>] [--<param> <value>]
       iunctus runs
       iunctus export <run> [-output <folder>] [-license <url>]

options:
  -lang nextflow|snakemake|cwl|argo   the language, when the target does not tell it
  -executor simulator|local           simulator (the default) writes TES task documents
                                      and makes up the outputs; local runs containers
  -graph <file>                       writes the execution graph, as DOT for a .dot file
  -max-pending <n>                    lets up to n tasks be pending at once (default 1)
  -order asked|reversed|shuffled[:<seed>]
                                      the order in which pending tasks finish
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
