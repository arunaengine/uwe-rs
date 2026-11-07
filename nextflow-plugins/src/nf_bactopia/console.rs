//! What nf-bactopia prints: the banner with changed params or the help at run start, and the
//! execution summary at run end. uwe leaves out the plugin's logo and "Message of the Day".
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;

use nextflow_evaluator::interpreter::parse_json;
use nextflow_evaluator::value::{State as ObjectState, Value, tree};

use super::param;
use crate::Context;
use crate::nf_schema::{entries, get};

/// The core options of the banner, which are properties of the `workflow` object.
const CORE: [&str; 7] = [
    "runName",
    "launchDir",
    "workDir",
    "projectDir",
    "userName",
    "profile",
    "configFiles",
];

/// Prints the banner, or the help when `--help` or `--help_all` is set; `false` then ends the
/// run with success before its script, as the plugin exits with code 0.
pub(crate) fn start(context: &mut Context) -> bool {
    let all = param(context, "help_all").is_some();
    let mut text = header(context);
    if all || param(context, "help").is_some() {
        text.push_str(&help(context, all));
        context.host.print(&text);
        return false;
    }
    let groups = changed(context);
    let width = (groups.iter().flat_map(|(_, params)| params.iter()))
        .map(|(name, _)| name.len())
        .chain(CORE.iter().map(|name| name.len()))
        .max()
        .unwrap_or_default();
    for (title, params) in &groups {
        text.push_str(&format!("{title}\n"));
        for (name, value) in params {
            text.push_str(&format!("  {name:<width$}: {value}\n"));
        }
        text.push('\n');
    }
    text.push_str("Core Nextflow options\n");
    for label in CORE {
        let value = match metadata(context, label) {
            Value::List(list) => {
                let items: Vec<String> = list.borrow().items.iter().map(Value::to_string).collect();
                items.join(", ")
            }
            Value::Null => String::new(),
            other => other.to_string(),
        };
        text.push_str(&format!("  {label:<width$}: {value}\n"));
    }
    text.push_str("\n!! Only displaying parameters that differ from the defaults !!\n");
    text.push_str(&format!("{}\n\n", "-".repeat(68)));
    context.host.print(&text);
    true
}

/// Prints the execution summary of the run, which the help ended early does not have.
pub(crate) fn finish(context: &mut Context) {
    if param(context, "help").is_some() || param(context, "help_all").is_some() {
        return;
    }
    let success = metadata(context, "success").truth();
    let manifest = |name: &str| setting(context, &["manifest", name]);
    let nextflow = get(&metadata(context, "nextflow"), "version").unwrap_or(Value::Null);
    let workflow = get(&context.params, "workflow").and_then(|w| get(&w, "name"));
    let outdir = param(context, "outdir").unwrap_or_default();
    let mut lines = vec![
        (
            "Workflow",
            workflow.map(|name| name.to_string()).unwrap_or_default(),
        ),
        ("Bactopia Version", manifest("version")),
        ("Nextflow Version", nextflow.to_string()),
        ("Command Line", metadata(context, "commandLine").to_string()),
        ("Launch Dir", metadata(context, "launchDir").to_string()),
        ("Profile", metadata(context, "profile").to_string()),
        ("Completed At", completed(&metadata(context, "complete"))),
        ("Duration", metadata(context, "duration").to_string()),
        ("Resumed", metadata(context, "resume").truth().to_string()),
        ("Success", success.to_string()),
    ];
    if success {
        let merged = get(&context.params, "merge_folder").unwrap_or(Value::Null);
        lines.push(("Merged Results", format!("{outdir}/{merged}")));
    }
    let mut text = format!(
        "\n{}\n\nBactopia Execution Summary\n{}\n",
        "-".repeat(68),
        "-".repeat(31)
    );
    for (label, value) in lines {
        text.push_str(&format!("{label:<17}: {value}\n"));
    }
    let name = get(&context.params, "workflow").and_then(|w| get(&w, "name"));
    if success && name.is_some_and(|name| name.to_string() == "bactopia") {
        let profile = metadata(context, "profile");
        let command = format!("bactopia -profile {profile} --bactopia {outdir} --wf");
        text.push_str(&format!(
            "\n\nFurther analyze your samples using Bactopia Tools, with the following command:\n{}\n{command} <REPLACE_WITH_BACTOPIA_TOOL_NAME>\n\nExamples:\n",
            "-".repeat(80)
        ));
        for tool in ["pangenome", "merlin", "sccmec"] {
            text.push_str(&format!("{command} {tool}\n"));
        }
        text.push_str("\nSee the full list of available Bactopia Tools: bactopia --list_wfs\n\n");
    }
    context.host.print(&text);
}

/// The top of the banner and the help: the pipeline's name, version and description.
fn header(context: &Context) -> String {
    let manifest = |name: &str| setting(context, &["manifest", name]);
    let workflow = get(&context.params, "workflow").unwrap_or(Value::Null);
    let description = get(&workflow, "description")
        .map(|d| d.to_string())
        .unwrap_or_default();
    let line = "-".repeat(45);
    format!(
        "\n{line}\n  {} v{}\n  {description} \n{line}\n",
        manifest("name"),
        manifest("version")
    )
}

/// The parameter groups of the schema: title and parameters with their definitions.
fn groups(context: &Context) -> Vec<(String, Vec<(String, Value)>)> {
    let file = match context.config.get(&["bactopia", "parametersSchema"]) {
        Some(Value::String(file)) => file.to_string(),
        _ => "nextflow_schema.json".into(),
    };
    let path = PathBuf::from(context.project_dir).join(file);
    let Ok(schema) = std::fs::read_to_string(&path).map(|text| parse_json(&text)) else {
        return Vec::new();
    };
    let schema = schema.unwrap_or(Value::Null);
    let defined = get(&schema, "$defs").or_else(|| get(&schema, "definitions"));
    let mut found = Vec::new();
    for (_, group) in entries(&defined.unwrap_or(Value::Null)) {
        let title = get(&group, "title")
            .map(|t| t.to_string())
            .unwrap_or_default();
        let properties = entries(&get(&group, "properties").unwrap_or(Value::Null));
        found.push((title, properties));
    }
    found
}

/// The params of each group whose value differs from the schema's default.
fn changed(context: &Context) -> Vec<(String, Vec<(String, String)>)> {
    let mut shown = Vec::new();
    for (title, properties) in groups(context) {
        let mut params = Vec::new();
        for (name, definition) in properties {
            let value = get(&context.params, &name).unwrap_or(Value::Null);
            let default = get(&definition, "default").unwrap_or(Value::Null);
            if !matches!(value, Value::Null) && value.to_string() != default.to_string() {
                params.push((name, value.to_string()));
            }
        }
        if !params.is_empty() {
            shown.push((title, params));
        }
    }
    shown
}

/// The help: `--help` and `--help_all`, then each group's params, hidden ones only for all.
fn help(context: &Context, all: bool) -> String {
    let groups = groups(context);
    let hidden = |definition: &Value| get(definition, "hidden").is_some_and(|h| h.truth());
    let visible = |definition: &Value| all || !hidden(definition);
    let (mut width, mut kinds) = (0, "[boolean]".len());
    for (_, properties) in &groups {
        for (name, definition) in properties.iter().filter(|(_, d)| visible(d)) {
            width = width.max(name.len() + 2);
            let kind = get(definition, "type")
                .map(|k| k.to_string())
                .unwrap_or_default();
            kinds = kinds.max(kind.len() + 2);
        }
    }
    // A name column as wide as the longest param; the two options stand where the indent is.
    let row = |name: &str, indent: usize, kind: &str, description: &str, default: &str| {
        let (name, kind) = (format!("--{name}"), format!("[{kind}]"));
        let width = width + indent;
        format!("{name:<width$} {kind:<kinds$} {description} {default}")
    };
    let mut text = format!(
        "\n{}\n{}\n\n",
        row(
            "help",
            2,
            "boolean",
            "Show this message for all non-hidden parameters.",
            ""
        ),
        row(
            "help_all",
            2,
            "boolean",
            "Show the help message for all parameters.",
            ""
        ),
    );
    let mut count = 0;
    for (title, properties) in &groups {
        text.push_str(&format!("{title}\n"));
        for (name, definition) in properties {
            if !visible(definition) {
                count += 1;
                continue;
            }
            let kind = get(definition, "type")
                .map(|k| k.to_string())
                .unwrap_or_default();
            let description = get(definition, "description")
                .map(|d| d.to_string())
                .unwrap_or_default();
            let default = match get(definition, "default") {
                Some(Value::Null) | None => String::new(),
                Some(default) => format!("[default: {default}] "),
            };
            text.push_str(&format!(
                "  {}\n",
                row(name, 0, &kind, &description, &default)
            ));
        }
    }
    let hiding = match count {
        0 => String::new(),
        count => format!(" !! Hiding {count} param(s), use `--help_all` to show them !!"),
    };
    text.push_str(&format!("\n{hiding}\n\n"));
    text
}

/// A property of the run's `workflow` object.
fn metadata(context: &Context, name: &str) -> Value {
    let Value::Object(object) = &context.workflow else {
        return Value::Null;
    };
    match &object.borrow().state {
        ObjectState::Workflow(workflow) => {
            tree::get(&workflow.properties, &[name]).unwrap_or(Value::Null)
        }
        _ => Value::Null,
    }
}

fn setting(context: &Context, path: &[&str]) -> String {
    context
        .config
        .get(path)
        .map(|value| value.to_string())
        .unwrap_or_default()
}

/// The end of the run as an ISO date and time with the offset of the local time zone.
fn completed(complete: &Value) -> String {
    let Value::Date(millis) = complete else {
        return String::new();
    };
    let Ok(stamp) = jiff::Timestamp::from_millisecond(*millis) else {
        return String::new();
    };
    let zoned = stamp.to_zoned(jiff::tz::TimeZone::system());
    let fraction = match millis.rem_euclid(1000) {
        0 => String::new(),
        millis => format!(".{millis:03}"),
    };
    format!(
        "{}{fraction}{}",
        zoned.strftime("%Y-%m-%dT%H:%M:%S"),
        zoned.strftime("%:z")
    )
}
