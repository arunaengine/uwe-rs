//! The `workflow` and `nextflow` objects of a run.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::rc::Rc;

use nextflow_config::tree;
use nextflow_evaluator::date;
use nextflow_evaluator::number::Number;
use nextflow_evaluator::value::{Object, PathValue, State, Value, Workflow};

use crate::Options;

/// The Nextflow version that iunctus follows, as `nextflow.version` shows it.
pub const NEXTFLOW_VERSION: &str = "26.04.6";

/// `nextflow.script.WorkflowMetadata` for a run that starts now.
pub(crate) fn metadata(options: &Options, launch_dir: &str) -> Value {
    let path = |text: &str| Value::Path(Rc::new(PathValue::parse(text, launch_dir)));
    let text = |text: &str| Value::string(text);
    let variable = |name: &str| std::env::var(name).unwrap_or_default();
    let output_dir = match options.config.get(&["outputDir"]) {
        Some(Value::Null) | None => path("results"),
        Some(directory) => path(&directory.to_string()),
    };
    let script = PathValue::parse(&options.script_file, launch_dir);
    let project_name = script
        .parent()
        .map(|parent| parent.name().to_owned())
        .unwrap_or_default();
    let manifest = options
        .config
        .get(&["manifest"])
        .map_or_else(tree::map, |manifest| tree::deep_copy(&manifest));
    let config_files = options.config_files.iter().map(|file| path(file)).collect();
    let engine = options
        .container_engine
        .as_deref()
        .map_or(Value::Null, Value::string);
    let stats = [
        "succeededCount",
        "cachedCount",
        "failedCount",
        "ignoredCount",
        "pendingCount",
        "submittedCount",
        "runningCount",
        "retriesCount",
        "abortedCount",
    ]
    .map(|name| (text(name), Value::integer(0)));
    let properties = Value::map([
        (text("runName"), text(&options.run_name)),
        (text("sessionId"), text(&options.run_name)),
        (text("start"), Value::Date(date::now())),
        (text("complete"), Value::Null),
        (text("duration"), Value::Null),
        (text("success"), Value::Boolean(false)),
        (text("exitStatus"), Value::Null),
        (text("errorMessage"), Value::Null),
        (text("errorReport"), Value::Null),
        (text("launchDir"), path(launch_dir)),
        (text("workDir"), path("work")),
        (text("projectDir"), path(&options.project_dir)),
        (text("outputDir"), output_dir),
        (text("homeDir"), path(&variable("HOME"))),
        (text("userName"), text(&variable("USER"))),
        (text("profile"), text(&options.profile)),
        (text("configFiles"), Value::list(config_files)),
        (text("commandLine"), text(&options.command_line)),
        (text("scriptFile"), Value::Path(Rc::new(script.clone()))),
        (text("scriptName"), text(script.name())),
        (text("scriptId"), Value::Null),
        (text("projectName"), Value::string(project_name)),
        (text("repository"), Value::Null),
        (text("commitId"), Value::Null),
        (text("revision"), Value::Null),
        (text("container"), Value::Null),
        (text("containerEngine"), engine),
        (text("stubRun"), Value::Boolean(options.stub)),
        (text("resume"), Value::Boolean(false)),
        (text("preview"), Value::Boolean(false)),
        (text("failOnIgnore"), Value::Boolean(false)),
        (text("manifest"), manifest),
        (text("nextflow"), nextflow()),
        (text("stats"), Value::map(stats)),
    ]);
    Value::Object(Rc::new(RefCell::new(Object {
        class: "nextflow.script.WorkflowMetadata",
        state: State::Workflow(Workflow {
            properties,
            on_complete: Vec::new(),
            on_error: Vec::new(),
        }),
    })))
}

/// `nextflow`: the version of Nextflow that iunctus follows.
pub(crate) fn nextflow() -> Value {
    Value::map([
        (Value::string("version"), Value::string(NEXTFLOW_VERSION)),
        (Value::string("build"), Value::integer(0)),
    ])
}

/// Records the end of a run: `success`, `complete`, `duration`, `exitStatus` and the task
/// counts.
pub(crate) fn finish(workflow: &Value, success: bool, succeeded: i64) {
    let Value::Object(object) = workflow else {
        return;
    };
    let object = object.borrow();
    let State::Workflow(workflow) = &object.state else {
        return;
    };
    let now = date::now();
    let set = |name: &str, value: Value| {
        tree::assign(&workflow.properties, &[name.into()], value).expect("a map");
    };
    let start = match tree::get(&workflow.properties, &["start"]) {
        Some(Value::Date(start)) => start,
        _ => now,
    };
    set("success", Value::Boolean(success));
    set("complete", Value::Date(now));
    set("duration", Value::Duration(now - start));
    set("exitStatus", Value::integer(if success { 0 } else { 1 }));
    let path = ["stats".into(), "succeededCount".into()];
    let count = Value::Number(Number::Long(succeeded));
    tree::assign(&workflow.properties, &path, count).expect("a map");
}

/// The handlers of `workflow.onComplete`, or of `workflow.onError`, in order.
pub(crate) fn handlers(
    workflow: &Value,
    on_error: bool,
) -> Vec<Rc<nextflow_evaluator::value::Closure>> {
    let Value::Object(object) = workflow else {
        return Vec::new();
    };
    match &object.borrow().state {
        State::Workflow(workflow) if on_error => workflow.on_error.clone(),
        State::Workflow(workflow) => workflow.on_complete.clone(),
        _ => Vec::new(),
    }
}
