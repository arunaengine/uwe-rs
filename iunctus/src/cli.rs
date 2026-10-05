//! Command-line adapter over the library run path.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub use crate::arguments::{Arguments, help, parse_command};
pub use crate::failure::{Failure, USAGE};
pub use crate::logging::init;
use crate::repeat::Pinned;
pub use crate::setup::Defaults;
use crate::setup::Setup;
use crate::{
    changes, check, clean, conda, inputs, insight, machine, order, page, repeat, report, runs,
};
use crate::{execute, failure, finish, launch, nextflow, run::Ran};
use std::path::Path;
use workflow_graph::ExecutionGraph;
use workflow_graph::render::{Format, Options, render};

/// What an error of the command line points to.
pub const HELP: &str = "`iunctus help` lists the commands and their options";

pub fn command(command: &str, arguments: &Arguments, usage: &str) -> Result<(), Failure> {
    match command {
        "run" => run(arguments),
        "graph" => graph(arguments),
        "check" => check::command(arguments),
        "check-machine" => machine::command(arguments),
        "config" => config(arguments),
        "runs" => runs::list(arguments),
        "export" => runs::export(arguments),
        "page" => page::command(arguments),
        "why" => runs::why(arguments),
        "origin" => runs::origin(arguments),
        "inputs" => inputs::command(arguments),
        "outputs" => inputs::outputs(arguments),
        "clean" => clean::command(arguments),
        "check-order" => order::command(arguments),
        "changes" => changes::command(arguments),
        "repeat" => repeat::command(arguments),
        "lock-environment" => conda::command(arguments),
        "time" => insight::time::command(arguments),
        "compare" => insight::compare::command(arguments),
        "replay" => insight::replay::command(arguments),
        "telemetry" => insight::telemetry::command(arguments),
        "report" => report::command(arguments),
        "help" | "-help" | "--help" => {
            println!("{usage}");
            Ok(())
        }
        _ => Err(Failure::usage(format!(
            "unknown command `{command}`; {HELP}"
        ))),
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
    if arguments.executor.as_deref() == Some("simulator") {
        println!(
            "iunctus: simulated run selected; outputs are made-up files, no task containers run"
        );
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
