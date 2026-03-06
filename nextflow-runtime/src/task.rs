//! A Nextflow task as a task-model request: `.command.sh` with the code that captures `env`
//! and `eval` outputs, the hashed specification, and the placement from the directives.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, HashMap};

use nextflow_evaluator::interpreter::TaskDirective;
use nextflow_evaluator::value::Value;
use task_model::{
    Digest, EnvironmentValue, FORMAT, FileKind, Image, InputFile, OutputRule, Placement,
    ReuseScope, TaskSpec,
};

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

/// The task-model request of a task. `image` is the container reference, pinned to a digest if
/// possible; `script` is the digest of `command_script`, and `inputs` the digest of each staged
/// file, in the order of `request.staged`.
pub fn task_request(
    request: &TaskRequest,
    image: Option<String>,
    script: Digest,
    inputs: &[Digest],
) -> task_model::TaskRequest {
    let mut files = vec![InputFile {
        path: SCRIPT.into(),
        kind: FileKind::File,
        digest: script,
        executable: false,
        writable: false,
    }];
    for ((_, name), digest) in request.staged.iter().zip(inputs) {
        files.push(InputFile {
            path: name.clone(),
            kind: FileKind::File,
            digest: digest.clone(),
            executable: false,
            writable: false,
        });
    }
    let mut outputs: Vec<OutputRule> = request
        .output_patterns
        .iter()
        .map(|pattern| OutputRule {
            pattern: pattern.clone(),
        })
        .collect();
    if !request.env.is_empty() || !request.eval.is_empty() {
        outputs.push(OutputRule {
            pattern: ENVIRONMENT.into(),
        });
    }
    let mut arguments: Vec<String> = match directive(&request.directives, "shell") {
        Some(Value::List(list)) => list.borrow().items.iter().map(Value::to_string).collect(),
        Some(Value::String(shell)) => vec![shell.to_string()],
        _ => vec!["/bin/bash".into(), "-ue".into()],
    };
    arguments.push(SCRIPT.into());
    let mut environment = BTreeMap::new();
    environment.insert(
        "NXF_TASK_WORKDIR".to_owned(),
        EnvironmentValue::WorkDirectory,
    );
    let spec = TaskSpec {
        format: FORMAT,
        namespace: String::new(),
        image: image.map(|reference| Image {
            reference,
            platform: None,
        }),
        arguments,
        environment,
        inputs: files,
        outputs,
        stdin: None,
        stdout: STDOUT.into(),
        stderr: STDERR.into(),
    };
    let scope = if spec.image_pinned() {
        ReuseScope::Shared
    } else {
        ReuseScope::Lineage
    };
    task_model::TaskRequest {
        identity: request.name.clone(),
        display: request.name.clone(),
        scope,
        attempt: 1,
        spec,
        placement: placement(&request.directives),
    }
}

/// The container image a task asks for, from its `container` directive.
pub fn container(request: &TaskRequest) -> Option<String> {
    match directive(&request.directives, "container") {
        Some(Value::String(image)) if !image.is_empty() => Some(image.to_string()),
        _ => None,
    }
}

fn placement(directives: &[TaskDirective]) -> Placement {
    let number = |name: &str| match directive(directives, name) {
        Some(Value::Number(number)) => Some(number.to_i64().max(0) as u64),
        Some(Value::MemoryUnit(bytes)) => Some(bytes.max(0) as u64),
        Some(Value::Duration(millis)) => Some(millis.max(0) as u64 / 1_000),
        _ => None,
    };
    Placement {
        cpus: number("cpus").unwrap_or(1) as u32,
        memory_bytes: number("memory"),
        disk_bytes: number("disk"),
        time_seconds: number("time"),
        ..Placement::default()
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
