//! Builds container scripts, shells, images, and resources from Nextflow task directives.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;

use nextflow_evaluator::interpreter::{TaskDirective, normalise_script};
use nextflow_evaluator::number::Number;
use nextflow_evaluator::units::{parse_duration, parse_memory};
use nextflow_evaluator::value::Value;
use workflow_graph::Resources;

use crate::process::TaskRequest;

/// The file names Nextflow gives the files of a task.
pub const SCRIPT: &str = ".command.sh";
pub const STDOUT: &str = ".command.out";
pub const STDERR: &str = ".command.err";
pub const ENVIRONMENT: &str = ".command.env";
pub const SECRET_NAMES: &str = "UWE_SECRET_NAMES";
pub const SECRET_FILE: &str = "/run/uwe/secrets";

pub fn secrets(
    directives: &[TaskDirective],
) -> Result<Vec<String>, nextflow_evaluator::interpreter::Error> {
    let mut names = std::collections::BTreeSet::new();
    for directive in directives
        .iter()
        .filter(|directive| &*directive.name == "secret")
    {
        for value in &directive.arguments {
            let values = match value {
                Value::List(list) => list.borrow().items.clone(),
                Value::Null => Vec::new(),
                value => vec![value.clone()],
            };
            for value in values {
                let name = value.to_string();
                if name.is_empty()
                    || name.bytes().enumerate().any(|(index, byte)| {
                        !(byte == b'_'
                            || byte.is_ascii_alphabetic()
                            || index > 0 && byte.is_ascii_digit())
                    })
                {
                    return Err(nextflow_evaluator::interpreter::Error::Unsupported {
                        message: "process directive `secret` with an invalid environment name"
                            .into(),
                        span: Default::default(),
                    });
                }
                names.insert(name);
            }
        }
    }
    Ok(names.into_iter().collect())
}

/// The task script, followed by uwe's own code that captures `env` and `eval` outputs into
/// `.command.env` in Nextflow's format. The script alone equals Nextflow's up to that code.
pub fn command_script(request: &TaskRequest) -> String {
    if request.env.is_empty() && request.eval.is_empty() {
        return request.script.clone();
    }
    let mut script = request.script.clone();
    script.push_str("\n# capture process environment\nset +u\nset +e\n");
    script.push_str(concat!(
        "uwe_eval() {\n",
        "    local output status\n",
        "    output=$(bash -c \"$2\" 2> .command.eval)\n",
        "    status=$?\n",
        "    if [ \"$status\" -ne 0 ]; then output=$(cat .command.eval); fi\n",
        // Nextflow reads the output with `IFS=$'\n' read`, which drops leading newlines too.
        "    output=\"${output#\"${output%%[!$'\\n']*}\"}\"\n",
        "    rm -f .command.eval\n",
        "    printf '%s=%s\\n/%s/=exit:%s\\n' \"$1\" \"$output\" \"$1\" \"$status\" >> .command.env\n",
        "}\n",
        "echo '' > .command.env\n",
    ));
    for name in &request.env {
        script.push_str(&format!(
            "echo {name}=\"${{{name}[@]}}\" >> .command.env\necho /{name}/ >> .command.env\n"
        ));
    }
    for (index, command) in request.eval.iter().enumerate() {
        let quoted = format!("'{}'", command.replace('\'', r"'\''"));
        script.push_str(&format!("uwe_eval nxf_out_eval_{} {quoted}\n", index + 1));
    }
    script
}

/// The program of a task, with `.command.sh` as its argument: the interpreter of the
/// script's own shebang, like `#!/usr/bin/env python` of a template, as Nextflow runs it;
/// else the shell of its `shell` directive, else Nextflow's default `/bin/bash -ue`.
pub fn command(request: &TaskRequest) -> Vec<String> {
    let shell = directive(&request.directives, "shell");
    let added = normalise_script("", shell.as_ref());
    let own = (request.script.lines().next())
        .filter(|line| line.starts_with("#!") && Some(*line) != added.lines().next())
        .map(|line| {
            line[2..]
                .split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .filter(|words| !words.is_empty());
    let mut arguments: Vec<String> = match (own, shell) {
        (Some(words), _) => words,
        (None, Some(Value::List(list))) => {
            list.borrow().items.iter().map(Value::to_string).collect()
        }
        (None, Some(Value::String(shell))) => vec![shell.to_string()],
        _ => vec!["/bin/bash".into(), "-ue".into()],
    };
    arguments.push(SCRIPT.into());
    arguments
}

pub fn launcher(request: &TaskRequest) -> Option<String> {
    let before = directive(&request.directives, "beforeScript")
        .filter(Value::truth)
        .map(|value| value.to_string());
    let after = directive(&request.directives, "afterScript")
        .filter(Value::truth)
        .map(|value| value.to_string());
    let secrets = secrets(&request.directives).unwrap_or_default();
    if before.is_none() && after.is_none() && secrets.is_empty() {
        return None;
    }
    let mut script = String::from(
        "set -e\nset +u\nuwe_status=0\ntrap 'uwe_last=$?; if [ \"$uwe_status\" -ne 0 ]; then exit \"$uwe_status\"; fi; exit \"$uwe_last\"' EXIT\n",
    );
    if let Some(before) = before {
        script.push_str(&format!("{{\n{before}\n}} >&2\n"));
    }
    script.push_str("set -u\nexport NXF_TASK_WORKDIR=\"$PWD\"\n");
    if !secrets.is_empty() {
        script.push_str(&format!("source {SECRET_FILE}\n"));
    }
    for (_, name) in &request.staged {
        let parent = std::path::Path::new(name)
            .parent()
            .unwrap_or_else(|| std::path::Path::new(""));
        let quote = |text: &str| format!("'{}'", text.replace('\'', "'\\''"));
        if !parent.as_os_str().is_empty() {
            script.push_str(&format!("mkdir -p {}\n", quote(&parent.to_string_lossy())));
        }
        script.push_str(&format!(
            "mv {} {}\n",
            quote(&format!(".command.stage/{name}")),
            quote(name)
        ));
    }
    script.push_str("set +e\n\"$@\"\nuwe_status=$?\n");
    if let Some(after) = after {
        script.push_str(&format!("{{\n{after}\n}} >&2\nuwe_after=$?\n[ \"$uwe_status\" -eq 0 ] && uwe_status=$uwe_after\n"));
    }
    script.push_str("exit \"$uwe_status\"\n");
    Some(script)
}

/// Whether a task gets copies of its input files, which it may change, as the directive
/// `stageInMode 'copy'` asks; otherwise it sees them read-only where they are.
pub fn copies_inputs(request: &TaskRequest) -> bool {
    matches!(directive(&request.directives, "stageInMode"), Some(Value::String(mode)) if &*mode == "copy")
}

/// The container image a task asks for, from its `container` directive.
pub fn container(request: &TaskRequest) -> Option<String> {
    if let Some(value) = directive(&request.directives, "conda").filter(Value::truth) {
        let source = value.to_string();
        return Some(if source.ends_with(".yml") || source.ends_with(".yaml") {
            let path = std::path::Path::new(&source);
            let path = if path.is_absolute() || source.contains("://") {
                path.to_owned()
            } else {
                std::env::current_dir().unwrap_or_default().join(path)
            };
            format!("conda-env:{}", path.display())
        } else {
            format!(
                "conda-env:nextflow-packages:{}",
                serde_json::to_string(&source).unwrap()
            )
        });
    }
    match directive(&request.directives, "container") {
        Some(Value::String(image)) if !image.is_empty() => Some(image.to_string()),
        _ => None,
    }
}

/// What a task gives the Wave service: its `container` directive with the engine's registry,
/// and its `conda` and `arch` directives.
pub fn wave_sources(
    request: &TaskRequest,
    registry: Option<&str>,
    channels: &[String],
) -> nextflow_plugins::WaveSources {
    let text = |name: &str| {
        let value = directive(&request.directives, name).filter(Value::truth);
        value.map(|value| value.to_string())
    };
    nextflow_plugins::WaveSources {
        process: request.process.to_string(),
        container: text("container").map(|image| with_registry(&image, registry)),
        conda: text("conda"),
        platform: text("arch"),
        channels: channels.to_vec(),
    }
}

/// Prepends the engine registry unless the image's first part contains a dot or colon.
/// For example, docker.registry = 'quay.io' qualifies biocontainers/fastqc.
pub fn with_registry(image: &str, registry: Option<&str>) -> String {
    if image.starts_with("conda-env:") {
        return image.to_owned();
    }
    let Some(registry) = registry.filter(|registry| !registry.is_empty()) else {
        return image.to_owned();
    };
    let own = image
        .split_once('/')
        .is_some_and(|(first, _)| first.contains(['.', ':']));
    match own {
        true => image.to_owned(),
        false => format!("{}/{image}", registry.trim_end_matches('/')),
    }
}

/// The resources a task asks for with its `cpus`, `memory`, `disk`, `time` and `accelerator`
/// directives. Text like `'2 GB'` or `'1h'` counts as Nextflow reads it.
pub fn resources(request: &TaskRequest) -> Result<Resources, String> {
    let directives = &request.directives;
    let number = |name: &str| -> Result<Option<u64>, String> {
        let Some(directive) = directives
            .iter()
            .rev()
            .find(|directive| &*directive.name == name)
        else {
            return Ok(None);
        };
        let Some(value) = directive.arguments.last() else {
            return Err(format!(
                "process `{}` has an invalid `{name}` directive: no value was given",
                request.process
            ));
        };
        let invalid = |detail: String| {
            format!(
                "process `{}` has an invalid `{name}` directive value `{value}`: {detail}",
                request.process
            )
        };
        let nonnegative = |value: i64| {
            if value < 0 {
                Err(invalid("resource values cannot be negative".into()))
            } else {
                Ok(value as u64)
            }
        };
        let number = match value {
            Value::Null => return Ok(None),
            Value::Number(number) => nonnegative(
                checked_i64(number)
                    .ok_or_else(|| invalid("value is outside the signed 64-bit range".into()))?,
            )?,
            Value::MemoryUnit(bytes) => nonnegative(*bytes)?,
            Value::Duration(millis) => nonnegative(*millis)? / 1_000,
            Value::String(text) => match name {
                "memory" | "disk" => {
                    nonnegative(parse_memory(text).map_err(|error| invalid(error.to_string()))?)?
                }
                "time" => {
                    nonnegative(parse_duration(text).map_err(|error| invalid(error.to_string()))?)?
                        / 1_000
                }
                _ => text
                    .trim()
                    .parse()
                    .map_err(|error: std::num::ParseIntError| invalid(error.to_string()))?,
            },
            _ => return Err(invalid("expected a number or unit value".into())),
        };
        if matches!(name, "cpus" | "accelerator") && number > i32::MAX as u64 {
            return Err(invalid(
                "value exceeds Nextflow's signed 32-bit count".into(),
            ));
        }
        Ok(Some(number))
    };
    Ok(Resources {
        cpus: number("cpus")?.map(|cpus| cpus.max(1) as f64),
        memory_bytes: number("memory")?,
        disk_bytes: number("disk")?,
        time_seconds: number("time")?,
        gpus: number("accelerator")?.map(|count| count as u32),
        queue: directive(directives, "queue")
            .filter(|value| !matches!(value, Value::Null))
            .map(|value| value.to_string())
            .filter(|queue| !queue.is_empty()),
        labels: match directive(directives, "resourceLabels") {
            Some(Value::Map(labels)) => (labels.borrow().entries.iter())
                .map(|(name, value)| (name.0.to_string(), value.to_string()))
                .collect(),
            _ => Default::default(),
        },
        options: executor_options(directives),
    })
}

/// The options of executors that the directives of a task give: all `pod` options in one list,
/// the last `nomadOptions`, `clusterOptions` and `machineType`, and the named options of
/// `accelerator`.
fn executor_options(
    directives: &[TaskDirective],
) -> std::collections::BTreeMap<String, serde_json::Value> {
    use serde_json::Value as Json;
    let mut options = std::collections::BTreeMap::new();
    let mut pod = Vec::new();
    for directive in directives
        .iter()
        .filter(|directive| &*directive.name == "pod")
    {
        for value in &directive.arguments {
            match crate::inputs::to_json(value) {
                Json::Array(items) => pod.extend(items),
                Json::Null => {}
                item => pod.push(item),
            }
        }
    }
    if !pod.is_empty() {
        options.insert("pod".to_owned(), Json::Array(pod));
    }
    for name in ["nomadOptions", "clusterOptions", "machineType"] {
        if let Some(value) = directive(directives, name) {
            let value = crate::inputs::to_json(&value);
            if !value.is_null() {
                options.insert(name.to_owned(), value);
            }
        }
    }
    let accelerator = (directives.iter().rev())
        .find(|directive| &*directive.name == "accelerator")
        .and_then(|directive| {
            (directive.arguments.iter()).find(|value| matches!(value, Value::Map(_)))
        });
    if let Some(named) = accelerator {
        options.insert("accelerator".to_owned(), crate::inputs::to_json(named));
    }
    options
}

/// Java's truncating `longValue`, but without wrapping large integers or saturating floats.
fn checked_i64(number: &Number) -> Option<i64> {
    match number {
        Number::Integer(value) => Some(i64::from(*value)),
        Number::Long(value) => Some(*value),
        Number::BigInteger(value) => value.to_string().parse().ok(),
        Number::BigDecimal(value) => value.to_bigint().to_string().parse().ok(),
        Number::Double(value) if value.is_finite() => (-9_223_372_036_854_775_808.0
            ..9_223_372_036_854_775_808.0)
            .contains(value)
            .then_some(*value as i64),
        Number::Float(value) if value.is_finite() => {
            let value = f64::from(*value);
            (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0)
                .contains(&value)
                .then_some(value as i64)
        }
        Number::Double(_) | Number::Float(_) => None,
    }
}

/// The last value of a directive.
fn directive(directives: &[TaskDirective], name: &str) -> Option<Value> {
    directives
        .iter()
        .rev()
        .find(|directive| &*directive.name == name)
        .and_then(|directive| directive.arguments.last().cloned())
}

/// The variables of `.command.env`: `NAME=value` up to the line `/NAME/`, which may carry the
/// exit status of an `eval` command as `/NAME/=exit:<status>`.
pub fn read_environment(text: &str) -> HashMap<String, (String, i32)> {
    let mut variables = HashMap::new();
    let mut current: Option<(String, String)> = None;
    for line in text.lines() {
        if let Some((name, value)) = &mut current {
            let end = format!("/{name}/");
            if let Some(rest) = line.strip_prefix(&end) {
                let status = rest.strip_prefix("=exit:").and_then(|s| s.parse().ok());
                if status.is_none() && name.starts_with("nxf_out_eval_") {
                    current = None;
                    continue;
                }
                variables.insert(name.clone(), (std::mem::take(value), status.unwrap_or(0)));
                current = None;
            } else {
                value.push('\n');
                value.push_str(line);
            }
            continue;
        }
        if let Some((name, value)) = line.split_once('=')
            && !name.is_empty()
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            current = Some((name.to_owned(), value.to_owned()));
        }
    }
    variables
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_engine_registry() {
        let quay = Some("quay.io");
        assert_eq!(
            with_registry("biocontainers/fastqc:0.12.1", quay),
            "quay.io/biocontainers/fastqc:0.12.1"
        );
        assert_eq!(with_registry("ubuntu:24.04", quay), "quay.io/ubuntu:24.04");
        let wave = "community.wave.seqera.io/library/fastqc:0.12.1";
        assert_eq!(with_registry(wave, quay), wave);
        assert_eq!(
            with_registry("localhost:5000/tool", quay),
            "localhost:5000/tool"
        );
        assert_eq!(with_registry("biocontainers/x", None), "biocontainers/x");
    }
}
