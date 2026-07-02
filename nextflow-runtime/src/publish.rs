//! Nextflow's `publishDir`: which output files of a finished task go where, and how. The engine
//! delivers them from the store.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};
use std::rc::Rc;

use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::{Error, Interpreter, TaskDirective, TaskFrame};
use nextflow_evaluator::value::{Key, PathValue, Value};
use workflow_graph::{Delivery, DeliveryMode};

use crate::process::glob_matches;

/// One `publishDir` of a task, with its path resolved.
pub(crate) struct Rule {
    path: PathBuf,
    mode: DeliveryMode,
    /// Only the files whose names match it, when given.
    pattern: Option<String>,
    /// A closure that names the published file, or leaves it out with `null`.
    save_as: Option<Value>,
    /// Whether a file already at the target is replaced.
    overwrite: bool,
}

/// The `publishDir` rules of a task: from each directive, a path with named options, a map of
/// options with `path`, or a list of such maps. A rule with `enabled: false` is left out.
/// Relative paths are relative to the launch directory.
pub(crate) fn rules(
    interpreter: &mut Interpreter,
    frame: &TaskFrame,
    task_object: &Value,
    directives: &[TaskDirective],
) -> Result<Vec<Rule>, Error> {
    let mut rules = Vec::new();
    for directive in directives.iter().filter(|d| &*d.name == "publishDir") {
        let mut options = Vec::new();
        let mut path = None;
        for argument in &directive.arguments {
            match argument {
                Value::Map(_) => options.push(argument.clone()),
                Value::List(list) => options.extend(list.borrow().items.iter().cloned()),
                other => path = Some(other.clone()),
            }
        }
        if options.is_empty() {
            options.push(Value::map([]));
        }
        for option in options {
            let get = |name: &str| option_of(&option, name);
            let enabled = get("enabled").is_none_or(|enabled| enabled.truth());
            let Some(target) = get("path").or_else(|| path.clone()) else {
                if enabled {
                    return Err(thrown("Missing `path` of the `publishDir` directive"));
                }
                continue;
            };
            if !enabled {
                continue;
            }
            let target = interpreter.resolve_setting(frame, &target, task_object)?;
            let mode = match get("mode") {
                Some(mode) => mode_of(&mode.to_string())?,
                None => DeliveryMode::Link,
            };
            rules.push(Rule {
                path: absolute(&path_text(&target)),
                mode,
                pattern: get("pattern").map(|pattern| pattern.to_string()),
                save_as: get("saveAs").filter(|save_as| !matches!(save_as, Value::Null)),
                overwrite: get("overwrite").is_none_or(|overwrite| overwrite.truth()),
            });
        }
    }
    Ok(rules)
}

/// Where each output of a finished task goes by the rules: each file whose name matches the
/// pattern, under the name that `saveAs` gives it, in the folder of the rule.
pub(crate) fn deliveries(
    interpreter: &mut Interpreter,
    frame: &TaskFrame,
    task_object: &Value,
    rules: &[Rule],
    files: &[(String, PathValue)],
) -> Result<Vec<Delivery>, Error> {
    let mut deliveries = Vec::new();
    for rule in rules {
        for (name, location) in files {
            if rule
                .pattern
                .as_ref()
                .is_some_and(|pattern| !glob_matches(pattern, name))
            {
                continue;
            }
            let saved = match &rule.save_as {
                Some(save_as) => {
                    let argument = vec![Value::string(name.as_str())];
                    interpreter.call_in_task(frame, task_object, save_as, argument)?
                }
                None => Value::string(name.as_str()),
            };
            if matches!(saved, Value::Null) || saved.to_string().is_empty() {
                continue;
            }
            let target = rule.path.join(path_text(&saved));
            if !rule.overwrite && target.exists() {
                continue;
            }
            deliveries.push(Delivery {
                location: location.path.clone(),
                target,
                mode: rule.mode,
            });
        }
    }
    Ok(deliveries)
}

/// How a mode of `publishDir` delivers. A move becomes a copy, since the store keeps the file,
/// and a copy that does not follow links is a copy, since stored files are no links.
fn mode_of(mode: &str) -> Result<DeliveryMode, Error> {
    Ok(match mode {
        "symlink" => DeliveryMode::Link,
        "rellink" => DeliveryMode::RelativeLink,
        "link" => DeliveryMode::HardLink,
        "copy" | "copyNoFollow" | "move" => DeliveryMode::Copy,
        other => return Err(thrown(&format!("Unknown publish mode: {other}"))),
    })
}

fn option_of(options: &Value, name: &str) -> Option<Value> {
    match options {
        Value::Map(map) => map.borrow().entries.get(&Key(Value::string(name))).cloned(),
        _ => None,
    }
}

/// The text of a path value: the path of a `Path`, else the string.
fn path_text(value: &Value) -> String {
    match value {
        Value::Path(path) => path.path.clone(),
        other => other.to_string(),
    }
}

fn absolute(path: &str) -> PathBuf {
    std::path::absolute(Path::new(path)).unwrap_or_else(|_| PathBuf::from(path))
}

fn thrown(message: &str) -> Error {
    Error::Thrown {
        exception: Rc::new(Exception::new("IllegalArgumentException", message)),
        span: Default::default(),
    }
}
