//! The inputs and outputs of a workflow as JSON Schema (decision 24): `iunctus inputs` prints the
//! schema of the inputs of any workflow, `iunctus outputs` that of its outputs, and every run
//! checks its values against the schema of its inputs before any task starts.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use input_schema::{Problem, Severity};
use workflow_graph::{Driver, Inputs, Language};

use crate::arguments::Arguments;
use crate::{Failure, failure};

/// Checks the inputs of a run before any task starts. Warnings go where the messages of the
/// workflow go: standard output, or standard error when `quiet` keeps standard output for the
/// graph. Errors fail the run, one line each. The result holds the inputs, when the driver
/// knows them.
pub fn check(
    driver: &mut dyn Driver,
    simulated: bool,
    quiet: bool,
) -> Result<Option<Inputs>, Failure> {
    let Some(inputs) = driver.inputs()? else {
        return Ok(None);
    };
    let problems = input_schema::check(&inputs, simulated);
    let say = |text: String| match quiet {
        true => eprintln!("{text}"),
        false => println!("{text}"),
    };
    for warning in warnings(&inputs, &problems) {
        say(format!("iunctus: warning: {warning}"));
    }
    let errors: Vec<String> = problems
        .iter()
        .filter(|problem| problem.severity == Severity::Error)
        .map(|problem| format!("iunctus: {problem}"))
        .collect();
    if !errors.is_empty() {
        return Err(failure(errors.join("\n")));
    }
    Ok(Some(inputs))
}

/// The warnings of a check, then where the schema and the inputs that the code reads differ.
fn warnings(inputs: &Inputs, problems: &[Problem]) -> Vec<String> {
    let mut warnings: Vec<String> = problems
        .iter()
        .filter(|problem| problem.severity == Severity::Warning)
        .map(|problem| problem.message.clone())
        .collect();
    if let Some(read) = &inputs.read {
        warnings.extend(input_schema::compare(&inputs.schema, read));
    }
    warnings
}

/// `iunctus inputs`: prints the JSON Schema of the inputs of a workflow, or with `-format text`
/// the inputs as help text. With `-check`, the values of the launch are checked as `iunctus run`
/// with the same options checks them: a simulated run, unless `-executor local` asks for a real
/// one. The workflow runs no task.
pub fn command(arguments: &Arguments) -> Result<(), Failure> {
    let text = text_format(arguments, "inputs")?;
    let (mut driver, language, simulated) = describing(arguments)?;
    let inputs = match arguments.check {
        true => check(driver.as_mut(), simulated, true)?,
        false => {
            let inputs = driver.inputs()?;
            if let Some(inputs) = &inputs {
                for warning in warnings(inputs, &[]) {
                    eprintln!("iunctus: warning: {warning}");
                }
            }
            inputs
        }
    };
    let Some(inputs) = inputs else {
        return Err(failure(format!(
            "iunctus: the {language} driver does not describe the inputs of its workflows yet"
        )));
    };
    print(&inputs.schema, text.then_some(input_schema::text))
}

/// `iunctus outputs`: prints the JSON Schema of the outputs of a workflow, the results of a run,
/// or with `-format text` the outputs as text. The workflow runs no task.
pub fn outputs(arguments: &Arguments) -> Result<(), Failure> {
    let text = text_format(arguments, "outputs")?;
    let (mut driver, language, _) = describing(arguments)?;
    let Some(schema) = driver.outputs()? else {
        return Err(failure(format!(
            "iunctus: the {language} driver does not describe the outputs of its workflows yet"
        )));
    };
    print(&schema, text.then_some(input_schema::outputs_text))
}

/// Whether `-format` asks a command that describes a workflow for text rather than JSON.
fn text_format(arguments: &Arguments, command: &str) -> Result<bool, Failure> {
    match arguments.format.as_deref() {
        None | Some("json") => Ok(false),
        Some("text") => Ok(true),
        Some(other) => Err(Failure::usage(format!(
            "iunctus: `-format {other}` is a format of `iunctus graph`; `iunctus {command}` \
             writes json or text"
        ))),
    }
}

/// The driver of the workflow that a command describes, its language, and whether a run with
/// the same options would be simulated.
fn describing(arguments: &Arguments) -> Result<(Box<dyn Driver>, Language, bool), Failure> {
    let (target, language) = crate::target_and_language(arguments)?;
    let launch = crate::launch(arguments, target);
    let setup = crate::setup::Setup::load(arguments)?;
    let run = crate::run_id();
    let driver = crate::driver(language, &launch, &run, true, arguments, &setup)?;
    Ok((driver, language, launch.simulated))
}

/// Prints a schema as JSON, or as text when a writer of text is given.
fn print(
    schema: &serde_json::Value,
    text: Option<fn(&serde_json::Value) -> String>,
) -> Result<(), Failure> {
    match text {
        Some(text) => print!("{}", text(schema)),
        None => {
            let json = serde_json::to_string_pretty(schema)
                .map_err(|error| failure(format!("iunctus: {error}")))?;
            println!("{json}");
        }
    }
    Ok(())
}
