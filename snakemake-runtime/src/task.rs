//! Turns a job into a task: the formatted shell command or the script, wrapper or `run:`
//! block, the container image, the files it reads and writes, and its resources.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use python_evaluator::{Args, Error, Interpreter, Value};
use workflow_graph::{Input, Resources, Task, TaskId};

use crate::files::WorkflowFiles;
use crate::functions::{call_input_function, is_callable};
use crate::objects::NamedList;
use crate::pattern;
use crate::plan::{Job, incomplete_target};
use crate::rule::{Action, Items, Rule};
use crate::shell::format_command;
use crate::workflow::{State, workflow_error};

/// Where wrappers come from unless the workflow names a URL.
pub const WRAPPER_PREFIX: &str = "https://github.com/snakemake/snakemake-wrappers/raw/";

/// Plain texts of items, as `Namedlist._plainstrings` gives them to functions.
fn plain(items: &Items, kind: &'static str) -> Value {
    let values = items
        .values
        .iter()
        .map(|value| match value.as_str() {
            Some(text) => Value::str(text),
            None => value.clone(),
        })
        .collect();
    NamedList {
        values,
        names: items.names.clone(),
        kind,
    }
    .value()
}

/// The value of a directive that may be a function of wildcards and other job values.
fn evaluate(
    interpreter: &mut Interpreter,
    value: &Value,
    wildcards: &Value,
    aux: &[(&str, Value)],
) -> Result<Value, Error> {
    if is_callable(interpreter, value) {
        call_input_function(interpreter, value.plain(), wildcards, aux)
    } else {
        Ok(value.clone())
    }
}

/// Parses sizes like `10G` or `500 MB` into megabytes, as Snakemake's sized resources do.
fn megabytes(text: &str) -> Option<f64> {
    let text = text.trim();
    let split = text
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let number: f64 = number.trim().parse().ok()?;
    let factor = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "m" | "mb" => 1.0,
        "k" | "kb" => 1e-3,
        "g" | "gb" => 1e3,
        "t" | "tb" => 1e6,
        "p" | "pb" => 1e9,
        "kib" | "ki" => 1024.0 / 1e6,
        "mib" | "mi" => 1024.0 * 1024.0 / 1e6,
        "gib" | "gi" => 1024.0 * 1024.0 * 1024.0 / 1e6,
        "tib" | "ti" => 1024f64.powi(4) / 1e6,
        _ => return None,
    };
    Some(number * factor)
}

/// Parses a runtime in minutes or with units like `2h` or `1d 3h` into seconds.
fn seconds(value: &Value) -> Option<u64> {
    if let Some(minutes) = python_evaluator::number::to_f64(value) {
        return Some((minutes * 60.0) as u64);
    }
    let text = value.as_str()?.trim().to_ascii_lowercase();
    let mut total = 0.0;
    let mut number = String::new();
    for c in text.chars() {
        if c.is_ascii_digit() || c == '.' {
            number.push(c);
            continue;
        }
        if c.is_whitespace() {
            continue;
        }
        let amount: f64 = number.parse().ok()?;
        number.clear();
        total += amount
            * match c {
                'w' => 604800.0,
                'd' => 86400.0,
                'h' => 3600.0,
                'm' => 60.0,
                's' => 1.0,
                _ => return None,
            };
    }
    if !number.is_empty() {
        total += number.parse::<f64>().ok()? * 60.0;
    }
    Some(total as u64)
}

fn task_resources(threads: i64, resources: &[(String, Value)]) -> Resources {
    let mut result = Resources {
        cpus: Some(threads.max(1) as f64),
        ..Resources::default()
    };
    let number = |value: &Value| python_evaluator::number::to_f64(value);
    for (name, value) in resources {
        match name.as_str() {
            "mem_mb" => result.memory_bytes = number(value).map(|mb| (mb * 1e6) as u64),
            "mem_mib" => {
                result.memory_bytes = number(value).map(|mib| (mib * 1024.0 * 1024.0) as u64)
            }
            "mem_gb" => result.memory_bytes = number(value).map(|gb| (gb * 1e9) as u64),
            "mem" if result.memory_bytes.is_none() => {
                result.memory_bytes = match value.as_str() {
                    Some(text) => megabytes(text).map(|mb| (mb * 1e6) as u64),
                    None => number(value).map(|mb| (mb * 1e6) as u64),
                };
            }
            "disk_mb" => result.disk_bytes = number(value).map(|mb| (mb * 1e6) as u64),
            "disk_mib" => {
                result.disk_bytes = number(value).map(|mib| (mib * 1024.0 * 1024.0) as u64)
            }
            "disk" if result.disk_bytes.is_none() => {
                result.disk_bytes = match value.as_str() {
                    Some(text) => megabytes(text).map(|mb| (mb * 1e6) as u64),
                    None => number(value).map(|mb| (mb * 1e6) as u64),
                };
            }
            "runtime" | "time_min" | "walltime" => result.time_seconds = seconds(value),
            "gpu" | "gpus" | "nvidia_gpu" => result.gpus = number(value).map(|count| count as u32),
            _ => {}
        }
    }
    result
}

/// A container image as TES expects it: `docker://` URIs lose their scheme.
fn image_name(text: &str) -> String {
    text.strip_prefix("docker://").unwrap_or(text).to_owned()
}

/// The path of a source file of a rule, relative to the working directory when it is inside,
/// and its absolute location.
fn source_file(rule: &Rule, path: &str, workdir: &Path) -> (String, String) {
    if path.contains("://") {
        let name = path.rsplit('/').next().unwrap_or(path).to_owned();
        return (name, path.to_owned());
    }
    let absolute = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        rule.basedir.join(path)
    };
    let absolute = python_evaluator::host::normalize(&absolute);
    let relative = absolute
        .strip_prefix(workdir)
        .map(|relative| relative.to_string_lossy().into_owned())
        .unwrap_or_else(|_| {
            format!(
                ".snakemake/scripts/{}",
                absolute
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default()
            )
        });
    (relative, absolute.to_string_lossy().into_owned())
}

/// The program that runs a script, by its file extension, as Snakemake's script support.
fn script_command(path: &str) -> Result<Vec<String>, Error> {
    let lower = path.to_ascii_lowercase();
    let command: Vec<&str> = if lower.ends_with(".py") {
        vec!["python", path]
    } else if lower.ends_with(".r") {
        vec!["Rscript", "--vanilla", path]
    } else if lower.ends_with(".rmd") {
        return Ok(vec![
            "Rscript".into(),
            "--vanilla".into(),
            "-e".into(),
            format!("rmarkdown::render('{path}')"),
        ]);
    } else if lower.ends_with(".jl") {
        vec!["julia", path]
    } else if lower.ends_with(".sh") {
        vec!["bash", path]
    } else if lower.ends_with(".rs") {
        vec!["rust-script", path]
    } else if lower.ends_with(".xsh") {
        vec!["xonsh", path]
    } else if lower.ends_with(".hy") {
        vec!["hy", path]
    } else {
        return Err(workflow_error(format!(
            "Unsupported script: Expecting either Python (.py), R (.R), RMarkdown (.Rmd), Julia (.jl), Rust (.rs), Bash (.sh), Xonsh (.xsh) or Hy (.hy) script ({path})."
        )));
    };
    Ok(command.into_iter().map(str::to_owned).collect())
}

/// What the session knows that tasks need besides the job.
pub struct TaskSetting<'a> {
    pub state: &'a State,
    pub files: &'a WorkflowFiles,
    pub workdir: &'a Path,
}
