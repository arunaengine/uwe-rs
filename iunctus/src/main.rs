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
mod conda_backend;
mod detect;
mod failure;
mod fetched;
mod inputs;
mod insight;
mod kubernetes;
mod local;
mod logging;
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
mod run;
mod run_backend;
mod run_driver;
mod run_support;
mod runs;
mod setup;
mod slurm;
mod sources;
mod terminal;
mod tes;
mod tracked;

use std::path::Path;
use std::process::ExitCode;

use workflow_graph::render::{Format, Options, render};
use workflow_graph::{Execution, ExecutionGraph};

use crate::arguments::Arguments;
use crate::failure::{Failure, failure};
use crate::repeat::Pinned;
use crate::run::{Ran, close, execute};
use crate::run_backend::{executor, network_rule, recorded_workflow};
use crate::run_driver::{driver, launch, target_and_language};
use crate::run_support::{
    cache_directory, finish, run_id, runner_settings, stop_on_interrupt, use_cache_folder,
};
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
    if matches!(command.as_str(), "help" | "-help" | "--help") {
        return exit(match rest {
            [] => {
                println!("{USAGE}");
                Ok(())
            }
            [command] => arguments::help(command)
                .map(|text| println!("{text}"))
                .map_err(Failure::usage),
            _ => Err(Failure::usage("use `iunctus help [command]`")),
        });
    }
    if matches!(command.as_str(), "-version" | "--version" | "version") && rest.is_empty() {
        println!("iunctus {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    let mut arguments = match arguments::parse_command(command, rest) {
        Ok(arguments) => arguments,
        Err(message) => return exit(Err(Failure::usage(format!("{message}; {HELP}")))),
    };
    if arguments.help {
        return exit(
            arguments::help(command)
                .map(|text| println!("{text}"))
                .map_err(Failure::usage),
        );
    }
    if arguments.version || command == "version" {
        println!("iunctus {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    if let Err(failure) = logging::init(&arguments) {
        return exit(Err(failure));
    }
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
    let simulated = arguments.executor.as_deref().unwrap_or("simulator") == "simulator";
    if simulated {
        println!("iunctus: simulated run selected; use `-executor local` to run tasks for real");
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
    let result = finish(execution);
    if simulated {
        println!("iunctus: simulated run ended; use `-executor local` to run tasks for real");
    }
    result
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

/// `iunctus config`: the config of a Nextflow project, by default the launch directory.
fn config(arguments: &Arguments) -> Result<(), Failure> {
    let setup = Setup::load(arguments)?;
    let project = arguments.target.clone().unwrap_or_else(|| ".".into());
    nextflow::print_config(&launch(arguments, project), &setup.storage)?;
    Ok(())
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
       iunctus version
       iunctus help [command]

options:
  -help                               lists the options of this command
  -version, --version                 prints the crate version
  -v, -verbose                        enables debug logging on stderr; IUNCTUS_LOG sets the
                                      filter otherwise (default: warn)
  -quiet                              suppresses operational logging
  -machine-retries <n>                retries a task after a machine failure (default: 2)
  -machine-retry-delay-ms <n>          base retry delay in milliseconds (default: 1000),
                                      doubled with jitter and capped at one minute
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
  -profile <names>, -c <file>         Nextflow or Snakemake profiles and config files
  -params-file <file>, -inputs <file> a params file, or a CWL job file; as with cwltool,
                                      `run tool.cwl job.yml` takes job.yml as the job file
  -entry <name>                       the entry workflow or target
  -configfile <file>                  a Snakemake config file
  --config <name>=<value>...          Snakemake config values
  --<param> <value>                   a workflow parameter; Snakemake uses --config name=value
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

#[cfg(test)]
mod tests {
    use super::*;
    use workflow_graph::{DriverError, Engine, Launch};

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
