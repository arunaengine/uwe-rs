//! The `workflow` and `nextflow` objects of a run.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::rc::Rc;

use nextflow_config::{Config, tree};
use nextflow_evaluator::date;
use nextflow_evaluator::interpreter::{Host, Interpreter};
use nextflow_evaluator::number::Number;
use nextflow_evaluator::value::{Key, Object, PathValue, State, Value, Workflow};

use crate::Options;

/// The Nextflow version that uwe follows, as `nextflow.version` shows it.
pub const NEXTFLOW_VERSION: &str = "26.04.6";

/// The containers of the `process` scope, as Nextflow's `fetchContainers` gives them. A closure
/// runs with empty params and without `workflow` and `task`, where it shows as
/// `(dynamic resolved)`.
fn containers(config: &Config) -> Value {
    struct Quiet;
    impl Host for Quiet {
        fn print(&mut self, _: &str) {}
    }
    let resolved = |value: &Value| match value {
        Value::Closure(closure) => {
            let mut host = Quiet;
            let mut interpreter = Interpreter::new(&mut host);
            interpreter.set_global("params", Value::map([]));
            match interpreter.call_closure(closure, Vec::new()) {
                Ok(value) => Value::string(value.to_string()),
                Err(_) => Value::string("(dynamic resolved)"),
            }
        }
        other => Value::string(other.to_string()),
    };
    let Some(process @ Value::Map(_)) = config.get(&["process"]) else {
        return Value::map([]);
    };
    let mut found = Vec::new();
    if let Value::Map(map) = &process {
        for (name, value) in &map.borrow().entries {
            let name = name.0.to_string();
            let name = name.strip_prefix("withName:").unwrap_or(&name).to_owned();
            if let Value::Map(_) = value
                && let Some(container) = config_entry(value, "container").filter(Value::truth)
            {
                found.push((Value::string(name), resolved(&container)));
            }
        }
    }
    match config_entry(&process, "container").filter(Value::truth) {
        Some(container) if found.is_empty() => resolved(&container),
        Some(container) => {
            found.push((Value::string("default"), resolved(&container)));
            Value::map(found)
        }
        None => Value::map(found),
    }
}

/// The value of a key of a config map.
fn config_entry(map: &Value, key: &str) -> Option<Value> {
    match map {
        Value::Map(map) => (map.borrow().entries)
            .get(&Key(Value::string(key)))
            .cloned(),
        _ => None,
    }
}

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
    let manifest = manifest(options);
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
        (text("container"), containers(&options.config)),
        (text("containerEngine"), engine),
        (text("stubRun"), Value::Boolean(options.stub)),
        (text("resume"), Value::Boolean(options.resume)),
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

/// `workflow.manifest`, as Nextflow's `Manifest` holds the `manifest` settings: most of them
/// as text, so a list of DOIs becomes the text of the list.
fn manifest(options: &Options) -> Value {
    let setting = |name: &str| options.config.get(&["manifest", name]);
    let text = |name: &str| match setting(name) {
        None | Some(Value::Null) => Value::Null,
        Some(value) => Value::string(value.to_string()),
    };
    let mut entries = Vec::new();
    for name in [
        "author",
        "defaultBranch",
        "description",
        "docsUrl",
        "doi",
        "homePage",
        "icon",
        "license",
        "mainScript",
        "name",
        "nextflowVersion",
        "organization",
        "version",
    ] {
        let value = match (name, text(name)) {
            ("mainScript", Value::Null) => Value::string("main.nf"),
            (_, value) => value,
        };
        entries.push((Value::string(name), value));
    }
    let contributors = setting("contributors").unwrap_or_else(|| Value::list(Vec::new()));
    entries.push((Value::string("contributors"), contributors));
    entries.push((
        Value::string("gitmodules"),
        setting("gitmodules").unwrap_or(Value::Null),
    ));
    let recurse = setting("recurseSubmodules").is_some_and(|value| value.truth());
    entries.push((Value::string("recurseSubmodules"), Value::Boolean(recurse)));
    Value::map(entries)
}

/// `nextflow`: the version of Nextflow that uwe follows.
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

/// Sets the start of a run, in milliseconds since the epoch, which its duration counts from.
pub(crate) fn started(workflow: &Value, start: i64) {
    let Value::Object(object) = workflow else {
        return;
    };
    let object = object.borrow();
    if let State::Workflow(workflow) = &object.state {
        let start = Value::Date(start);
        tree::assign(&workflow.properties, &["start".into()], start).expect("a map");
    }
}

pub(crate) fn failure(workflow: &Value, message: &str, status: Value) {
    let Value::Object(object) = workflow else {
        return;
    };
    let object = object.borrow();
    let State::Workflow(workflow) = &object.state else {
        return;
    };
    for (name, value) in [
        ("exitStatus", status),
        ("errorMessage", Value::string(message)),
        ("errorReport", Value::string(message)),
    ] {
        tree::assign(&workflow.properties, &[name.into()], value).expect("a map");
    }
}

/// A property of the `workflow` object.
pub(crate) fn property(workflow: &Value, name: &str) -> Value {
    let Value::Object(object) = workflow else {
        return Value::Null;
    };
    match &object.borrow().state {
        State::Workflow(workflow) => {
            tree::get(&workflow.properties, &[name]).unwrap_or(Value::Null)
        }
        _ => Value::Null,
    }
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
