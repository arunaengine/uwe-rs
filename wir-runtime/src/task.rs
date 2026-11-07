//! Task ops: the uwe task of a wir `task` op, and the outputs that its captures read from
//! what the task made.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::{Map, Value as Json, json};
use wir::{Argument, CaptureRule, Op, Port, Type, TypeDef, resolve};
use workflow_graph::simulator::glob_matches;
use workflow_graph::{Input, Outcome, OutputFile, Resources, Reuse, Task, TaskId};

/// Where a task runs in its container: its fresh working folder.
pub const WORKDIR: &str = "/work";

/// The task of a task op with its input values. Inputs are staged below `inputs/<port>`,
/// and the attributes only set what no cache key covers.
pub fn build(id: TaskId, path: &str, op: &Op, spec: &wir::Task, bound: &Map<String, Json>) -> Task {
    let mut inputs = Vec::new();
    let mut words = Vec::new();
    for argument in &spec.command {
        match argument {
            Argument::Text(text) => words.push(text.clone()),
            Argument::Input { input } => {
                let value = bound.get(input).unwrap_or(&Json::Null);
                words.extend(texts(value, &format!("inputs/{input}"), &mut inputs));
            }
        }
    }
    let stream = |rule: CaptureRule| {
        (spec.captures.iter())
            .find(|capture| capture.from == rule)
            .map(|capture| stream_name(&capture.output, &rule))
    };
    let (stdout, stderr) = (stream(CaptureRule::Stdout), stream(CaptureRule::Stderr));
    let mut outputs: Vec<String> = stdout.iter().chain(&stderr).cloned().collect();
    for capture in &spec.captures {
        if let CaptureRule::Glob(pattern) = &capture.from
            && !outputs.contains(pattern)
        {
            outputs.push(pattern.clone());
        }
    }
    let redirections: Vec<String> = [(">", &stdout), ("2>", &stderr)]
        .into_iter()
        .filter_map(|(sign, name)| name.as_ref().map(|name| format!("{sign} {}", quote(name))))
        .collect();
    let (command, script) = match redirections.is_empty() {
        true => (words, None),
        false => {
            let quoted: Vec<String> = words.iter().map(|word| quote(word)).collect();
            let script = format!("exec {}\n{}", redirections.join(" "), quoted.join(" "));
            (
                vec!["/bin/sh".into(), "-c".into(), script.clone()],
                Some(script),
            )
        }
    };
    let attributes = &op.attributes;
    let wanted = attributes.resources.clone().unwrap_or_default();
    Task {
        id,
        name: path.to_owned(),
        step: path.to_owned(),
        image: spec.image.clone(),
        command,
        script,
        workdir: Some(WORKDIR.into()),
        inputs,
        outputs,
        resources: Resources {
            cpus: wanted.cpus.map(f64::from),
            memory_bytes: wanted.memory,
            disk_bytes: wanted.disk,
            time_seconds: attributes.time,
            gpus: wanted.gpus,
            ..Resources::default()
        },
        reuse: match attributes.cache {
            Some(wir::Cache::Never) => Reuse::Disabled,
            _ => Reuse::Allowed,
        },
        ..Task::default()
    }
}

/// The output values that the captures of a task read from an outcome, or why the attempt
/// is not accepted: an exit code outside the success codes, or a capture that failed.
pub fn capture(
    spec: &wir::Task,
    ports: &[Port],
    outcome: &Outcome,
    defs: &[TypeDef],
) -> Result<Map<String, Json>, String> {
    let codes: &[i32] = match spec.success_codes.is_empty() {
        true => &[0],
        false => &spec.success_codes,
    };
    if outcome.error.is_some() || !codes.contains(&outcome.exit_code) {
        return Err(format!(
            "the command ended with exit code {}",
            outcome.exit_code
        ));
    }
    let mut values = Map::new();
    for port in ports {
        let Some(capture) = spec.captures.iter().find(|item| item.output == port.name) else {
            return Err(format!("output {} has no capture", port.name));
        };
        let found: Vec<&OutputFile> = match &capture.from {
            CaptureRule::Glob(pattern) => {
                let mut found: Vec<&OutputFile> = (outcome.files.iter())
                    .filter(|file| glob_matches(pattern, &file.path))
                    .collect();
                found.sort_by(|a, b| a.path.cmp(&b.path));
                found
            }
            rule => {
                let name = stream_name(&port.name, rule);
                outcome
                    .files
                    .iter()
                    .filter(|file| file.path == name)
                    .collect()
            }
        };
        let value = fill(&port.r#type, &found, defs)
            .map_err(|problem| format!("output {}: {problem}", port.name))?;
        if !wir::fits(&value, &port.r#type, defs) {
            return Err(format!("output {} does not fit its type", port.name));
        }
        values.insert(port.name.clone(), value);
    }
    Ok(values)
}

/// The value of an output of `r#type` from the files its capture found.
fn fill(r#type: &Type, found: &[&OutputFile], defs: &[TypeDef]) -> Result<Json, String> {
    match resolve(r#type, defs) {
        Some(Type::Array(_)) => Ok(Json::Array(found.iter().map(|file| entry(file)).collect())),
        Some(Type::Optional(_)) if found.is_empty() => Ok(Json::Null),
        Some(Type::Optional(inner)) => fill(inner, found, defs),
        _ => match found {
            [one] => Ok(entry(one)),
            [] => Err("no file matched".into()),
            _ => Err(format!("{} files matched one value", found.len())),
        },
    }
}

/// A File or Directory value of a file that a task made.
fn entry(file: &OutputFile) -> Json {
    let class = if file.directory { "Directory" } else { "File" };
    let basename = file
        .path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default();
    let mut value = json!({"class": class, "location": file.location, "basename": basename});
    if let (false, Some(size)) = (file.directory, file.size) {
        value["size"] = json!(size);
    }
    if let (false, Some(digest)) = (file.directory, &file.digest) {
        value["checksum"] = json!(digest);
    }
    value
}

/// The arguments of an input value, staging its files and folders below `folder`.
fn texts(value: &Json, folder: &str, inputs: &mut Vec<Input>) -> Vec<String> {
    match value {
        Json::Null => Vec::new(),
        Json::Bool(flag) => vec![flag.to_string()],
        Json::Number(number) => vec![number.to_string()],
        Json::String(text) => vec![text.clone()],
        Json::Array(items) => (items.iter().enumerate())
            .flat_map(|(index, item)| texts(item, &format!("{folder}/{index}"), inputs))
            .collect(),
        Json::Object(_) => match stage(value, folder, inputs) {
            Some(path) => vec![path],
            None => vec![value.to_string()],
        },
    }
}

/// Stages a File or Directory value and its secondary files in `folder`; its path there.
fn stage(value: &Json, folder: &str, inputs: &mut Vec<Input>) -> Option<String> {
    let class = value.get("class").and_then(Json::as_str)?;
    let directory = match class {
        "File" => false,
        "Directory" => true,
        _ => return None,
    };
    let location = value.get("location").and_then(Json::as_str);
    let basename = (value.get("basename").and_then(Json::as_str))
        .or_else(|| location.and_then(|text| text.trim_end_matches('/').rsplit('/').next()))
        .unwrap_or("input");
    let path = format!("{folder}/{basename}");
    let source = match location {
        Some(text) => cwl_parser::iri::file_path(text)
            .map_or(text.to_owned(), |path| path.to_string_lossy().into_owned()),
        None => String::new(),
    };
    let content = (value.get("contents").and_then(Json::as_str)).filter(|_| location.is_none());
    inputs.push(Input {
        path: path.clone(),
        source,
        directory,
        content: content.map(str::to_owned),
        ..Input::default()
    });
    for item in value
        .get("secondary_files")
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
    {
        stage(item, folder, inputs);
    }
    Some(path)
}

/// The file that a stream capture of an output writes to.
fn stream_name(output: &str, rule: &CaptureRule) -> String {
    match rule {
        CaptureRule::Stderr => format!("{output}.stderr"),
        _ => format!("{output}.stdout"),
    }
}

/// A word quoted for `/bin/sh`.
fn quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', r"'\''"))
}
