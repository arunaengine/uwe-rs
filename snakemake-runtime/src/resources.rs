//! Combines default, profile, and rule resources and converts memory and disk size units.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use python_evaluator::{Error, Interpreter, Scope, Value};
use workflow_graph::Resources;

use crate::objects::NamedList;
use crate::plan::incomplete_target;
use crate::rule::Rule;
use crate::task::evaluate as evaluate_value;
use crate::workflow::State;
use crate::workflow::workflow_error;

/// Snakemake's default resources for remote execution (`Resources.DEFAULTS["full"]`). uwe
/// sends tasks to executors like TES, so its jobs get them.
const DEFAULTS: &[(&str, &str)] = &[
    ("mem", "min(max(2*input.size_mb, 1000), 8000)"),
    ("disk", "max(2*input.size_mb, 1000) if input else 50000"),
    ("tmpdir", "system_tmpdir"),
];

/// Resources with a size, which exist in megabytes, human-readable and in mebibytes.
const SIZED: &[&str] = &["mem", "disk"];

/// The temporary folder of the machine that runs a task, for `system_tmpdir`.
const SYSTEM_TMPDIR: &str = "/tmp";

/// The values of a job that resource functions may take.
pub struct JobValues<'a> {
    pub wildcards: &'a Value,
    pub input: &'a Value,
    pub attempt: &'a Value,
    pub threads: i64,
    pub rulename: &'a Value,
}

/// The sized resource a name belongs to, like `mem` for `mem_mb`.
fn family(name: &str) -> Option<&'static str> {
    SIZED.iter().copied().find(|sized| {
        name == *sized
            || name
                .strip_prefix(sized)
                .is_some_and(|rest| rest == "_mb" || rest == "_mib")
    })
}

/// Sets a resource, replacing any value of the same sized resource in another unit.
fn set(resources: &mut Vec<(String, Value)>, name: &str, value: Value) {
    let group = family(name);
    resources
        .retain(|(existing, _)| existing != name && (group.is_none() || family(existing) != group));
    resources.push((name.to_owned(), value));
}

/// A resource value of the command line or a profile: a number, or a Python expression over
/// `input`, `threads`, `attempt`, `wildcards` and `system_tmpdir`; a text that is no
/// expression stays text, as Snakemake's `cli_evaluator` does.
fn expression(
    interpreter: &mut Interpreter,
    text: &str,
    job: &JobValues<'_>,
) -> Result<Value, Error> {
    let text = python_evaluator::host::expand_home(text);
    if let Ok(number) = text.trim().parse::<i64>() {
        return Ok(Value::Int(number));
    }
    if let Ok(number) = text.trim().parse::<f64>() {
        return Ok(Value::Float(number));
    }
    let scope = Scope::new(None);
    scope.set("wildcards", job.wildcards.clone());
    scope.set("input", job.input.clone());
    scope.set("attempt", job.attempt.clone());
    scope.set("threads", Value::Int(job.threads));
    scope.set("system_tmpdir", Value::str(SYSTEM_TMPDIR));
    match interpreter.eval_text(&text, &scope) {
        Ok(value) => Ok(value),
        Err(error) if error.is("NameError") || error.is("SyntaxError") => Ok(Value::str(text)),
        Err(error) => Err(workflow_error(format!(
            "Failed to evaluate resources value '{text}'. When interpreted as a python expression, the following error was given: {error}"
        ))),
    }
}

/// The resources of a job, by name: the defaults, the profile's defaults, the rule's, then
/// the profile's for the rule, with sized resources in all three units.
pub fn evaluate(
    interpreter: &mut Interpreter,
    rule: &Rule,
    state: &State,
    job: &JobValues<'_>,
) -> Result<Vec<(String, Value)>, Error> {
    let profile = &state.profile;
    let mut resources: Vec<(String, Value)> = Vec::new();
    for (name, text) in DEFAULTS {
        let value = expression(interpreter, text, job)?;
        set(&mut resources, name, value);
    }
    for (name, text) in &profile.default_resources {
        let value = expression(interpreter, text, job)?;
        set(&mut resources, name, value);
    }
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
        set(&mut resources, name, evaluated);
    }
    // A handover rule without resources of its own gets all global ones.
    if rule.handover && rule.resources.is_empty() {
        for (name, value) in &state.global_resources {
            set(&mut resources, name, value.clone());
        }
    }
    for (target, name, text) in &profile.set_resources {
        if *target == rule.name {
            let value = expression(interpreter, text, job)?;
            set(&mut resources, name, value);
        }
    }
    constrain(&mut resources, state, &rule.name)?;
    expand_sized(interpreter, resources)
}

/// Caps each integer resource at the global resource of its name, as Snakemake's
/// `Resource.constrain` does; another type than the global integer is an error.
fn constrain(resources: &mut [(String, Value)], state: &State, rule: &str) -> Result<(), Error> {
    for (name, value) in resources.iter_mut() {
        let Some(Value::Int(limit)) = state.global_resources.get(name.as_str()).map(Value::plain)
        else {
            continue;
        };
        match value.plain() {
            Value::Int(amount) => *value = Value::Int((*amount).min(*limit)),
            Value::None => {}
            other => {
                return Err(workflow_error(format!(
                    "Specified resource is of different type than global constraint provided by --resources:\n    {name} of type '{}' was constrained by mismatched type 'int' with value '{limit}'. (rule {rule})",
                    python_evaluator::types::type_name(other)
                )));
            }
        }
    }
    Ok(())
}

/// Megabytes, rounded, of a sized resource given in any unit.
fn in_megabytes(
    interpreter: &mut Interpreter,
    name: &str,
    value: &Value,
) -> Result<Option<i64>, Error> {
    let number = python_evaluator::number::to_f64(value);
    if name.ends_with("_mib") {
        return Ok(number.map(|mib| (mib.round() / 0.953_674_316_406_25).floor() as i64));
    }
    if name.ends_with("_mb") {
        return Ok(number.map(|mb| mb.round() as i64));
    }
    if let Some(number) = number {
        return Ok(Some(number.round() as i64));
    }
    let Some(text) = value.as_str() else {
        return Ok(None);
    };
    let stripped = text.trim_matches(['\'', '"']);
    if stripped.chars().all(|c| c.is_ascii_digit()) && !stripped.is_empty() {
        return Ok(stripped.parse().ok());
    }
    match megabytes(stripped) {
        Some(mb) => Ok(Some((mb.ceil() as i64).max(1))),
        None => Err(workflow_error(format!(
            "Resource '{name}' with value {} could not be parsed as size in MB",
            interpreter.repr(value)?
        ))),
    }
}

/// A size in megabytes as humanfriendly writes it, like `1 GB` or `1.5 GB`.
pub fn human_size(megabytes: i64) -> String {
    let bytes = megabytes as f64 * 1e6;
    for (unit, divider) in [
        ("PB", 1e15),
        ("TB", 1e12),
        ("GB", 1e9),
        ("MB", 1e6),
        ("KB", 1e3),
    ] {
        if bytes >= divider {
            let text = format!("{:.2}", bytes / divider);
            let text = text.trim_end_matches('0').trim_end_matches('.');
            return format!("{text} {unit}");
        }
    }
    format!("{bytes} bytes")
}

/// Sized resources as `<name>_mb`, followed by the human-readable `<name>` and `<name>_mib`,
/// and `runtime` in minutes, as Snakemake's `Resources.expand_items` gives them to jobs.
fn expand_sized(
    interpreter: &mut Interpreter,
    resources: Vec<(String, Value)>,
) -> Result<Vec<(String, Value)>, Error> {
    let mut expanded = Vec::with_capacity(resources.len() + 4);
    for (name, value) in resources {
        if let Some(sized) = family(&name)
            && let Some(megabytes) = in_megabytes(interpreter, &name, &value)?
        {
            expanded.push((format!("{sized}_mb"), Value::Int(megabytes)));
            expanded.push((sized.to_owned(), Value::str(human_size(megabytes))));
            let mebibytes = (megabytes as f64 * 0.953_674_316_406_25).ceil() as i64;
            expanded.push((format!("{sized}_mib"), Value::Int(mebibytes)));
            continue;
        }
        if name == "runtime"
            && let Some(text) = value.as_str()
        {
            let Some(minutes) = runtime_minutes(text) else {
                return Err(workflow_error(format!(
                    "Resource '{name}' with value {} could not be parsed as minutes",
                    interpreter.repr(&value)?
                )));
            };
            expanded.push((name, Value::Int(minutes)));
            continue;
        }
        expanded.push((name, value));
    }
    Ok(expanded)
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

/// A runtime text in minutes as Snakemake parses it: whole minutes, or one number of seconds
/// or of a time unit, like `90`, `2h` or `30 minutes`, as humanfriendly's `parse_timespan`
/// reads it. `None` when it cannot be parsed.
fn runtime_minutes(text: &str) -> Option<i64> {
    let stripped = text.trim_matches(['\'', '"']);
    if !stripped.is_empty() && stripped.chars().all(|c| c.is_ascii_digit()) {
        return stripped.parse().ok();
    }
    // humanfriendly's tokens: numbers, and the texts between them.
    let mut tokens: Vec<Result<f64, String>> = Vec::new();
    let mut rest = stripped;
    while !rest.is_empty() {
        let digits = rest
            .find(|c: char| c.is_ascii_digit())
            .unwrap_or(rest.len());
        let text = rest[..digits].trim();
        if !text.is_empty() {
            tokens.push(Err(text.to_owned()));
        }
        rest = &rest[digits..];
        if rest.is_empty() {
            break;
        }
        let whole = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        let mut end = whole;
        if rest[whole..].starts_with('.')
            && rest[whole + 1..].starts_with(|c: char| c.is_ascii_digit())
        {
            end = whole
                + 1
                + rest[whole + 1..]
                    .find(|c: char| !c.is_ascii_digit())
                    .unwrap_or(rest.len() - whole - 1);
        }
        tokens.push(Ok(rest[..end].parse().ok()?));
        rest = &rest[end..];
    }
    let seconds = match tokens.as_slice() {
        [Ok(number)] => *number,
        [Ok(number), Err(unit)] => {
            let divider = match unit.to_lowercase().as_str() {
                "ms" | "millisecond" | "milliseconds" => 1e-3,
                "s" | "sec" | "secs" | "second" | "seconds" => 1.0,
                "m" | "min" | "mins" | "minute" | "minutes" => 60.0,
                "h" | "hour" | "hours" => 3600.0,
                "d" | "day" | "days" => 86400.0,
                "w" | "week" | "weeks" => 604_800.0,
                "y" | "year" | "years" => 31_449_600.0,
                _ => return None,
            };
            number * divider
        }
        _ => return None,
    };
    Some(((seconds.round() / 60.0) as i64).max(1))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_resource_sizes() {
        assert_eq!(human_size(1000), "1 GB");
        assert_eq!(human_size(1500), "1.5 GB");
        assert_eq!(human_size(954), "954 MB");
        assert_eq!(human_size(16_000), "16 GB");
    }

    #[test]
    fn parses_resource_runtimes() {
        let cases = [
            ("2h", Some(120)),
            ("1.5h", Some(90)),
            ("'30 minutes'", Some(30)),
            ("1y", Some(524_160)),
            ("45", Some(45)),
            ("90s", Some(1)),
            ("2 H", Some(120)),
            ("1d 3h", None),
            ("03:00:00", None),
        ];
        for (text, minutes) in cases {
            assert_eq!(runtime_minutes(text), minutes, "{text}");
        }
    }

    #[test]
    fn replaces_sized_resources() {
        let mut resources = vec![
            ("mem".to_owned(), Value::Int(1000)),
            ("tmpdir".to_owned(), Value::str("/tmp")),
        ];
        set(&mut resources, "mem_mib", Value::Int(2000));
        let names: Vec<&str> = resources.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["tmpdir", "mem_mib"]);
    }

    #[test]
    fn converts_resource_units() {
        let mut interpreter =
            Interpreter::new(std::rc::Rc::new(python_evaluator::LocalHost::new("/")));
        let expanded = expand_sized(
            &mut interpreter,
            vec![
                ("mem".to_owned(), Value::str("2G")),
                ("disk_mib".to_owned(), Value::Int(954)),
                ("runtime".to_owned(), Value::str("2h")),
            ],
        )
        .expect("sizes parse");
        let text: Vec<String> = expanded
            .iter()
            .map(|(name, value)| format!("{name}={}", python_evaluator::format::plain_str(value)))
            .collect();
        assert_eq!(
            text,
            [
                "mem_mb=2000",
                "mem=2 GB",
                "mem_mib=1908",
                "disk_mb=1000",
                "disk=1 GB",
                "disk_mib=954",
                "runtime=120"
            ]
        );
    }
}
