//! The parameters of a pipeline against its schema, as nf-validation 1.1.3 checks,
//! summarises and explains them.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::value::Value;

use super::{Problem, flag, groups, parameters_schema};
use crate::Context;
use crate::nf_schema::{entries, get, json, paths, plain, thrown};

/// The params that nf-validation itself reads, which are never unexpected.
const OWN_PARAMS: &[&str] = &[
    "validationFailUnrecognisedParams",
    "validationLenientMode",
    "help",
    "validationShowHiddenParams",
    "validationSchemaIgnoreParams",
    "validationSkipDuplicateCheck",
    "validationS3PathCheck",
    "monochromeLogs",
    "monochrome_logs",
];

/// Options of `nextflow run`, which fail when given with two hyphens.
const NEXTFLOW_OPTIONS: &[&str] = &[
    "async",
    "bg",
    "cache",
    "config",
    "dsl1",
    "dsl2",
    "dump-channels",
    "dump-hashes",
    "entry",
    "hub",
    "latest",
    "lib",
    "main-script",
    "name",
    "offline",
    "params-file",
    "pi",
    "plugins",
    "poll-interval",
    "pool-size",
    "profile",
    "ps",
    "qs",
    "queue-size",
    "r",
    "resume",
    "revision",
    "stdin",
    "stub",
    "stub-run",
    "test",
    "user",
    "with-apptainer",
    "with-charliecloud",
    "with-conda",
    "with-dag",
    "with-docker",
    "with-mpi",
    "with-notification",
    "with-podman",
    "with-report",
    "with-singularity",
    "with-timeline",
    "with-tower",
    "with-trace",
    "with-weblog",
    "without-docker",
    "without-podman",
    "work-dir",
];

/// `validateParameters`: params that the schema does not know give a warning (an error
/// with `validationFailUnrecognisedParams`), the params are checked against the schema, and
/// each samplesheet that a param with a `schema` names is checked against that schema.
pub(super) fn validate(options: Option<&Value>, context: &mut Context) -> Result<Value, Error> {
    let schema = parameters_schema(options, context)?;
    let groups = groups(&schema.tree);
    let mut expected: Vec<String> = OWN_PARAMS.iter().map(|name| name.to_string()).collect();
    for (_, properties) in &groups {
        expected.extend(properties.iter().map(|(name, _)| name.clone()));
    }
    let ignored: Vec<String> = match get(&context.params, "validationSchemaIgnoreParams") {
        Some(value) if value.truth() => value
            .to_string()
            .split(',')
            .map(str::to_owned)
            .chain(["schemaIgnoreParams".to_owned()])
            .collect(),
        _ => Vec::new(),
    };
    let fail_unrecognised = flag(context, "validationFailUnrecognisedParams");
    let (mut problems, mut warnings) = (Vec::new(), Vec::new());
    for (name, value) in entries(&context.params) {
        if NEXTFLOW_OPTIONS.contains(&name.as_str()) {
            problems.push(format!(
                "You used a core Nextflow option with two hyphens: '--{name}'. Please resubmit \
                 with '-{name}'"
            ));
        }
        // A name with hyphens that matches a known name without them is the camel case of
        // the command line, not a param of its own.
        let folded = |name: &str| name.replace('-', "").to_lowercase();
        let camel_case = name.contains('-') && expected.iter().any(|e| folded(e) == folded(&name));
        if expected.contains(&name) || ignored.contains(&name) || camel_case {
            continue;
        }
        let line = format!("* --{name}: {value}");
        if fail_unrecognised {
            problems.push(line);
        } else {
            warnings.push(line);
        }
    }
    if !warnings.is_empty() {
        context.host.print(&format!(
            "WARN: The following invalid input values have been detected:\n\n{}\n\n",
            warnings.join("\n")
        ));
    }
    let instance = serde_json::Value::Object(
        entries(&context.params)
            .into_iter()
            .filter_map(|(name, value)| Some((name, json(&cleaned(&value)?))))
            .collect(),
    );
    let validator = paths::validator(&schema.json, &instance, context)
        .map_err(|error| thrown(format!("Invalid parameters schema: {error}")))?;
    for problem in super::problems(&validator, &instance) {
        problems.push(match problem {
            Problem::Missing { name } => format!("* Missing required parameter: --{name}"),
            Problem::Value {
                path,
                message,
                choices,
            } => {
                let name = path.trim_start_matches('/');
                let value = instance.get(name).map(plain).unwrap_or_default();
                match choices {
                    Some(choices) => format!(
                        "* --{name}: '{value}' is not a valid choice (Available choices{}",
                        listed(&choices)
                    ),
                    None if message.starts_with("the file or directory") => {
                        format!("* --{name}: {message}.")
                    }
                    None => format!("* --{name}: {message} ({value})"),
                }
            }
        });
    }
    if !problems.is_empty() {
        return Err(thrown(format!(
            "The following invalid input values have been detected:\n\n{}\n\n",
            problems.join("\n")
        )));
    }
    Ok(Value::Null)
}

/// The choices of an enum as nf-validation lists them: at most five, then `...`.
fn listed(choices: &[String]) -> String {
    const LIMIT: usize = 5;
    if choices.len() > LIMIT {
        format!(
            " ({LIMIT} of {}): {}, ... )",
            choices.len(),
            choices[..LIMIT].join(", ")
        )
    } else {
        format!(": {})", choices.join(", "))
    }
}

/// A param as nf-validation's `cleanParameters` leaves it: values that Groovy counts as false
/// are left out, and memory units, durations and maps become text.
fn cleaned(value: &Value) -> Option<Value> {
    if !value.truth() {
        return None;
    }
    Some(match value {
        Value::MemoryUnit(_) | Value::Duration(_) | Value::Map(_) => {
            Value::string(value.to_string())
        }
        other => other.clone(),
    })
}
