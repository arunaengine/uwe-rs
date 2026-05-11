//! The resources of a job: the values of the rule's `resources:` for its wildcards, input
//! and threads, and what the task asks of the executor.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use python_evaluator::{Error, Interpreter, Value};
use workflow_graph::Resources;

use crate::objects::NamedList;
use crate::plan::incomplete_target;
use crate::rule::Rule;
use crate::task::evaluate as evaluate_value;

/// The values of a job that resource functions may take.
pub struct JobValues<'a> {
    pub wildcards: &'a Value,
    pub input: &'a Value,
    pub attempt: &'a Value,
    pub threads: i64,
    pub rulename: &'a Value,
}

/// The rule's resources for a job, by name, in the order the rule gives them.
pub fn evaluate(
    interpreter: &mut Interpreter,
    rule: &Rule,
    job: &JobValues<'_>,
) -> Result<Vec<(String, Value)>, Error> {
    let mut resources: Vec<(String, Value)> = Vec::new();
    for (name, value) in &rule.resources {
        let evaluated = evaluate_value(
            interpreter,
            value,
            job.wildcards,
            &[
                ("input", job.input.clone()),
                ("attempt", job.attempt.clone()),
                ("threads", Value::Int(job.threads)),
                ("rulename", job.rulename.clone()),
            ],
        )
        .or_else(|error| {
            if incomplete_target(&error).is_some() {
                Ok(Value::Int(0))
            } else {
                Err(error)
            }
        })?;
        resources.push((name.clone(), evaluated));
    }
    Ok(resources)
}

/// The `resources` object of a job: `_cores` and the named resources.
pub fn named_list(threads: i64, resources: &[(String, Value)]) -> Value {
    let mut list = NamedList::new("Resources", Vec::new());
    list.values.push(Value::Int(threads));
    list.names.push(("_cores".into(), 0, None));
    for (name, value) in resources {
        list.values.push(value.clone());
        list.names.push((name.clone(), list.values.len() - 1, None));
    }
    list.value()
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

pub fn task_resources(threads: i64, resources: &[(String, Value)]) -> Resources {
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
