//! The parts of a Nextflow task that its container task takes: `.command.sh` with the code that
//! captures `env` and `eval` outputs, the shell, the image and the resources from the
//! directives.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;

use nextflow_evaluator::interpreter::TaskDirective;
use nextflow_evaluator::units::{parse_duration, parse_memory};
use nextflow_evaluator::value::Value;
use workflow_graph::Resources;

use crate::process::TaskRequest;

/// The file names Nextflow gives the files of a task.
pub const SCRIPT: &str = ".command.sh";
pub const STDOUT: &str = ".command.out";
pub const STDERR: &str = ".command.err";
pub const ENVIRONMENT: &str = ".command.env";

/// The task script, followed by iunctus's own code that captures `env` and `eval` outputs into
/// `.command.env` in Nextflow's format. The script alone equals Nextflow's up to that code.
pub fn command_script(request: &TaskRequest) -> String {
    if request.env.is_empty() && request.eval.is_empty() {
        return request.script.clone();
    }
    let mut script = request.script.clone();
    script.push_str("\n# capture process environment\nset +u\nset +e\n");
    script.push_str(concat!(
        "iunctus_eval() {\n",
        "    local output status\n",
        "    output=$(bash -c \"$2\" 2> .command.eval)\n",
        "    status=$?\n",
        "    if [ \"$status\" -ne 0 ]; then output=$(cat .command.eval); fi\n",
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
        script.push_str(&format!(
            "iunctus_eval nxf_out_eval_{} {quoted}\n",
            index + 1
        ));
    }
    script
}

/// The program of a task: the shell of its `shell` directive, else Nextflow's default
/// `/bin/bash -ue`, with `.command.sh` as its argument.
pub fn command(request: &TaskRequest) -> Vec<String> {
    let mut arguments: Vec<String> = match directive(&request.directives, "shell") {
        Some(Value::List(list)) => list.borrow().items.iter().map(Value::to_string).collect(),
        Some(Value::String(shell)) => vec![shell.to_string()],
        _ => vec!["/bin/bash".into(), "-ue".into()],
    };
    arguments.push(SCRIPT.into());
    arguments
}

/// The container image a task asks for, from its `container` directive.
pub fn container(request: &TaskRequest) -> Option<String> {
    match directive(&request.directives, "container") {
        Some(Value::String(image)) if !image.is_empty() => Some(image.to_string()),
        _ => None,
    }
}

/// An image with the registry of the container engine in front, as Nextflow puts it before
/// images without a registry of their own: `biocontainers/fastqc` becomes
/// `quay.io/biocontainers/fastqc` with `docker.registry = 'quay.io'`. An image names its own
/// registry when its first part holds a `.` or a `:`, like `community.wave.seqera.io/...`.
pub fn with_registry(image: &str, registry: Option<&str>) -> String {
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
pub fn resources(request: &TaskRequest) -> Resources {
    let directives = &request.directives;
    let number = |name: &str| match directive(directives, name) {
        Some(Value::Number(number)) => Some(number.to_i64().max(0) as u64),
        Some(Value::MemoryUnit(bytes)) => Some(bytes.max(0) as u64),
        Some(Value::Duration(millis)) => Some(millis.max(0) as u64 / 1_000),
        Some(Value::String(text)) => match name {
            "memory" | "disk" => parse_memory(&text).ok().map(|bytes| bytes.max(0) as u64),
            "time" => parse_duration(&text)
                .ok()
                .map(|millis| millis.max(0) as u64 / 1_000),
            _ => text.trim().parse().ok(),
        },
        _ => None,
    };
    Resources {
        cpus: number("cpus").map(|cpus| cpus as f64),
        memory_bytes: number("memory"),
        disk_bytes: number("disk"),
        time_seconds: number("time"),
        gpus: number("accelerator").map(|count| count as u32),
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
                let status = rest
                    .strip_prefix("=exit:")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                variables.insert(name.clone(), (std::mem::take(value), status));
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
    fn images_get_the_registry_of_the_engine() {
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
