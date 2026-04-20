//! The parameters of a pipeline against its schema, as nf-validation 1.1.3 checks,
//! summarises and explains them.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::value::Value;

use super::samplesheet::validate_file;
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
            Problem::Missing { name, .. } => format!("* Missing required parameter: --{name}"),
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
    for (_, properties) in &groups {
        for (name, definition) in properties {
            let Some(sheet) = get(&context.params, name).filter(Value::truth) else {
                continue;
            };
            if let Some(sheet_schema) = get(definition, "schema") {
                validate_file(name, &sheet, &sheet_schema, context)?;
            }
        }
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

/// `paramsSummaryMap`: the core options of the run first, then for each group of the
/// schema the params that differ from their defaults, as text.
pub(super) fn summary(
    options: Option<&Value>,
    workflow: &Value,
    context: &mut Context,
) -> Result<Value, Error> {
    let schema = parameters_schema(options, context)?;
    let mut core = Vec::new();
    let mut add = |name: &str, only_set: bool| {
        let value = get(workflow, name).unwrap_or(Value::Null);
        if !only_set || value.truth() {
            core.push((Value::string(name), value));
        }
    };
    add("revision", true);
    add("runName", false);
    add("containerEngine", true);
    add("container", true);
    for name in ["launchDir", "workDir", "projectDir", "userName", "profile"] {
        add(name, false);
    }
    let files = match get(workflow, "configFiles") {
        Some(Value::List(files)) => {
            let names: Vec<String> = files.borrow().items.iter().map(Value::to_string).collect();
            names.join(", ")
        }
        _ => String::new(),
    };
    core.push((Value::string("configFiles"), Value::string(files)));
    let outdir = get(&context.params, "outdir").map_or("null".into(), |o| o.to_string());
    let mut summary = vec![(Value::string("Core Nextflow options"), Value::map(core))];
    for (title, properties) in groups(&schema.tree) {
        let mut changed = Vec::new();
        for (name, definition) in properties {
            let Some(value) = get(&context.params, &name) else {
                continue;
            };
            let text = (!matches!(value, Value::Null)).then(|| value.to_string());
            let mut default = get(&definition, "default")
                .filter(|default| !matches!(default, Value::Null))
                .map(|default| default.to_string());
            let string = get(&definition, "type").is_some_and(|t| t.to_string() == "string");
            if let (Some(schema_value), Some(text), true) = (&default, &text, string) {
                // Defaults below `$projectDir` or `$params.outdir` count as their values.
                let project = schema_value
                    .replace("$projectDir", "")
                    .replace("${projectDir}", "");
                let below_project = project != *schema_value && text.contains(&project);
                let output = schema_value
                    .replace("$params.outdir", "")
                    .replace("${params.outdir}", "");
                let below_outdir = output != *schema_value && format!("{outdir}{output}") == *text;
                if below_project || below_outdir {
                    default = Some(text.clone());
                }
            }
            let differs = match (&default, &text) {
                (Some(default), text) => text.as_deref() != Some(default.as_str()),
                (None, Some(text)) => !text.is_empty() && text != "false",
                (None, None) => false,
            };
            if differs {
                let shown = text.map_or(Value::Null, Value::string);
                changed.push((Value::string(name), shown));
            }
        }
        summary.push((Value::string(title), Value::map(changed)));
    }
    Ok(Value::map(summary))
}

/// `paramsSummaryLog`: the summary as text, one group after another.
pub(super) fn summary_log(
    options: Option<&Value>,
    workflow: &Value,
    context: &mut Context,
) -> Result<Value, Error> {
    let summary = summary(options, workflow, context)?;
    let groups = entries(&summary);
    let width = groups
        .iter()
        .flat_map(|(_, group)| entries(group))
        .map(|(name, _)| name.chars().count())
        .max()
        .unwrap_or(0);
    let mut output = String::new();
    for (title, group) in groups {
        let params = entries(&group);
        if params.is_empty() {
            continue;
        }
        output.push_str(&format!("{title}\n"));
        for (name, value) in params {
            output.push_str(&format!("  {name:width$}: {value}\n"));
        }
        output.push('\n');
    }
    output.push_str("!! Only displaying parameters that differ from the pipeline defaults !!\n");
    output.push_str(&"-".repeat(54));
    Ok(Value::string(output))
}

/// `paramsHelp`: the command, then each group with its params, types and descriptions.
/// Hidden params are left out unless `validationShowHiddenParams` is set.
pub(super) fn help(
    options: Option<&Value>,
    command: &Value,
    context: &mut Context,
) -> Result<Value, Error> {
    let schema = parameters_schema(options, context)?;
    let groups = groups(&schema.tree);
    let width = groups
        .iter()
        .flat_map(|(_, properties)| properties)
        .map(|(name, _)| name.chars().count())
        .max()
        .unwrap_or(0)
        + 1;
    let show_hidden = flag(context, "validationShowHiddenParams");
    let mut output = format!("Typical pipeline command:\n\n  {command}\n\n");
    let mut hidden = 0;
    for (title, properties) in groups {
        let mut lines = Vec::new();
        for (name, definition) in properties {
            if get(&definition, "hidden").is_some_and(|h| h.truth()) && !show_hidden {
                hidden += 1;
                continue;
            }
            let kind = format!(
                "[{}]",
                get(&definition, "type").map_or("null".into(), |t| t.to_string())
            );
            let description =
                get(&definition, "description").map_or("null".into(), |d| d.to_string());
            let default = get(&definition, "default")
                .filter(|default| !matches!(default, Value::Null))
                .map_or(String::new(), |default| format!(" [default: {default}]"));
            lines.push(format!(
                "  --{name:width$}{kind:10}{description}{default}\n"
            ));
        }
        if !lines.is_empty() {
            output.push_str(&format!("{title}\n{}\n", lines.concat()));
        }
    }
    if hidden > 0 {
        output.push_str(&format!(
            " !! Hiding {hidden} params, use --validationShowHiddenParams to show them !!\n"
        ));
    }
    output.push_str(&"-".repeat(54));
    Ok(Value::string(output))
}
