//! The inputs of a workflow as one JSON Schema (decision 24): `iunctus inputs` prints the schema
//! of any workflow, and every run checks its values against it before any task starts.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use input_schema::{Problem, Severity};
use workflow_graph::{Driver, Inputs};

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
/// the inputs as help text. With `-check`, the values of the launch are checked as a real run
/// checks them. The workflow runs no task.
pub fn command(arguments: &Arguments) -> Result<(), Failure> {
    let text = match arguments.format.as_deref() {
        None | Some("json") => false,
        Some("text") => true,
        Some(other) => {
            return Err(failure(format!(
                "iunctus: `-format {other}` is a format of `iunctus graph`; `iunctus inputs` \
                 writes json or text"
            )));
        }
    };
    let (target, language) = crate::target_and_language(arguments)?;
    let mut launch = crate::launch(arguments, target);
    // The schema describes the inputs of a real run, without made-up values.
    launch.simulated = false;
    let setup = crate::setup::Setup::load(arguments)?;
    let run = crate::run_id();
    let mut driver = crate::driver(language, &launch, &run, true, arguments, &setup)?;
    let inputs = match arguments.check {
        true => check(driver.as_mut(), false, true)?,
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
    match text {
        true => print!("{}", input_schema::text(&inputs.schema)),
        false => {
            let json = serde_json::to_string_pretty(&inputs.schema)
                .map_err(|error| failure(format!("iunctus: {error}")))?;
            println!("{json}");
        }
    }
    Ok(())
}
