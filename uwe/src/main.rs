//! Dispatches CLI commands, the internal CWL JavaScript worker and the staging helper.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::process::ExitCode;
use uwe::cli::{self, Failure, HELP};

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
    // The helper that moves the files of a task on a node without shared folders.
    if arguments
        .first()
        .is_some_and(|command| command == "__stage")
    {
        return match executor::helper::main(&arguments[1..]) {
            Ok(()) => ExitCode::SUCCESS,
            Err(message) => {
                eprintln!("uwe: {message}");
                ExitCode::FAILURE
            }
        };
    }
    let Some((command, rest)) = arguments.split_first() else {
        eprintln!("{USAGE}");
        return ExitCode::from(cli::USAGE);
    };
    if matches!(command.as_str(), "help" | "-help" | "--help") {
        return exit(match rest {
            [] => {
                println!("{USAGE}");
                Ok(())
            }
            [command] => cli::help(command)
                .map(|text| println!("{text}"))
                .map_err(Failure::usage),
            _ => Err(Failure::usage("use `uwe help [command]`")),
        });
    }
    if matches!(command.as_str(), "-version" | "--version" | "version") && rest.is_empty() {
        println!("{}", env!("UWE_IDENTITY"));
        return ExitCode::SUCCESS;
    }
    let mut arguments = match cli::parse_command(command, rest) {
        Ok(arguments) => arguments,
        Err(message) => return exit(Err(Failure::usage(format!("{message}; {HELP}")))),
    };
    if arguments.help {
        return exit(
            cli::help(command)
                .map(|text| println!("{text}"))
                .map_err(Failure::usage),
        );
    }
    if arguments.version || command == "version" {
        println!("{}", env!("UWE_IDENTITY"));
        return ExitCode::SUCCESS;
    }
    if let Err(failure) = cli::init(&arguments) {
        return exit(Err(failure));
    }
    if !matches!(command.as_str(), "help" | "-help" | "--help") {
        match cli::Defaults::load(&arguments) {
            Ok(defaults) => defaults.apply(command, &mut arguments),
            Err(failure) => return exit(Err(failure)),
        }
    }
    let result = cli::command(command, &arguments, USAGE);
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

const USAGE: &str = "\
usage: uwe run <workflow file, directory or repository> [<targets>] [options]
       uwe graph <workflow file or directory> [-format json|dot|summary|svg] [-view tasks|steps]
                     [-direction down|right] [-fold <n>] [options]
       uwe check <workflow file or directory> [-default-image <image>] [options]
       uwe check-machine [-executor <executor>] [-store <folder or address>]
       uwe config [<project>] [-profile <names>] [-c <config>] [--<param> <value>]
       uwe runs
       uwe page [<run>] [-address <ip>] [-port <number>]
       uwe export <run> [-output <folder>] [-license <url>]
       uwe why <run> <task>
       uwe origin <file>
       uwe digest <path>
       uwe time <run> [-format text|json]
       uwe compare <run> [<run>] [-format text|json] [-report <file>] [-fail-on-change]
       uwe replay <run> <task> [-executor <executor>] [-store <place>]
       uwe telemetry <run> [-output <file>]
       uwe report <run> [-output <file>]
       uwe resume <run> [<step>] [<name>=<value>]...
       uwe reconcile <run> -reason <text> [-database <file>]
       uwe inputs <workflow file or directory> [-format json|text] [-check] [options]
       uwe outputs <workflow file or directory> [-format json|text] [options]
       uwe clean [-keep-days <n>] [-keep-run <run>]... [-keep-slower-than <time>] [-remove] [-front]
       uwe check-order <workflow file or directory> [options]
       uwe changes <run>
       uwe repeat <run> [-executor <executor>] [-store <folder or address>] [options]
       uwe lock-environment <conda environment file> [-platform <name>] [-output <file>]
       uwe kuberun <repository or path in the cluster> -head-image <image> [-kubeconfig <file>]
                     [-volume-mount <claim:path,...>] [-background] [options]
       uwe version
       uwe help [command]

options:
  -help                               lists the options of this command
  -version, --version                 prints the crate version
  -v, -verbose                        enables debug logging on stderr; UWE_LOG sets the
                                      filter otherwise (default: warn)
  -quiet                              suppresses operational logging
  -machine-retries <n>                retries a task after a machine failure (default: 2)
  -machine-retry-delay-ms <n>         base retry delay in milliseconds (default: 1000),
                                      doubled with jitter and capped at one minute
  -task-minutes <n>                   the time limit of tasks that set none of their own
  -lang nextflow|snakemake|cwl|argo   the language, when the target does not tell it
  -r <tag|branch|commit>, -latest     repository revision and refresh of its cached reference
  -executor <executor>                local (the default for run) runs Docker or Podman
                                      containers on this machine; simulator writes TES task
                                      documents and makes up outputs, tes runs on a TES server,
                                      slurm as SLURM batch jobs, kubernetes as Kubernetes Jobs,
                                      aws-batch, google-batch and azure-batch as jobs of these
                                      cloud services, nomad as Nomad jobs, htcondor and pbs as
                                      HTCondor and PBS jobs
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
                                      uwe never takes the default one
  -kubernetes-namespace <name>        the namespace of the Jobs; the kubeconfig's by default
  -head-image <image>                 the image with uwe on its PATH that `kuberun` runs in a
                                      pod; -head-cpus <n> and -head-memory <size> its requests
  -volume-mount <claim:path,...>      volume claims of the `kuberun` pod; the first holds the
                                      cache of uwe
  -background                         `kuberun` ends once the pod has started
  -max-pending <n>                    lets up to n tasks be pending at once (default 1 for
                                      the simulator, 1024 for real executors)
  -order asked|reversed|finished|shuffled[:<seed>]
                                      the order in which pending tasks finish (default
                                      asked for the simulator, finished for real executors)
  -store <folder or address>          the cache store of real executors, in a folder or at an
                                      address of a storage provider, like s3://bucket/cache
  -uwe-config <file>              a config file of uwe, read after
                                      /etc/uwe/config.toml and
                                      ~/.config/uwe/config.toml
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
  -strict-provenance                  `run` stops if durable provenance cannot be recorded
  -reason <text>                      explains why a stopped local owner is reconciled
  -no-right-sizing                    tasks ask for the CPUs and memory that the workflow
                                      names, not for what earlier runs of their step used
  -no-grouping                        runs each task in a container of its own; without it,
                                      short tasks of a step run one after another in one
                                      container
  -page                               serves the web page of the run while it goes on
  -report <file>                      writes the report of the run when it ends: one HTML
                                      file with the page of the run, to open without a server;
                                      for `compare`, the report of the comparison: JSON for
                                      .json, Markdown for .md, else one HTML page
  -fail-on-change                     `compare` ends with exit code 3 when the runs differ
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
  -front                              `clean` evicts only the configured local front store
  -no-network                         runs the tasks without network whose workflow does not
                                      say that they need it; `[network] allowed_steps` of the
                                      config file names steps that get it anyway
  -check-reproducibility <percent>    runs this share of the tasks whose results come from the
                                      cache again, like 10%, and compares their outputs; a tool
                                      that gives other bytes shares no results any more";
