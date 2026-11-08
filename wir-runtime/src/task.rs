//! Task ops: the uwe task of a wir `task` op, and the outputs that its captures read from
//! what the task made.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::{Map, Value as Json, json};
use wir::{Argument, CaptureRule, FileName, Op, Port, Type, TypeDef, resolve};
use workflow_graph::simulator::glob_matches;
use workflow_graph::{Files, Input, Network, Outcome, OutputFile, Resources, Reuse, Task, TaskId};

/// Where a task runs in its container: its fresh working folder.
pub const WORKDIR: &str = "/work";

/// The most bytes of a file that a capture loads as its contents, as in CWL.
const CONTENTS_LIMIT: usize = 64 * 1024;

/// The task of a task op with its input values. Inputs are staged below `inputs/<port>`,
/// and the attributes only set what no cache key covers.
pub fn build(
    id: TaskId,
    (path, step): (&str, &str),
    op: &Op,
    spec: &wir::Task,
    bound: &Map<String, Json>,
) -> Task {
    let mut inputs = Vec::new();
    let mut words = Vec::new();
    for argument in &spec.command {
        match argument {
            Argument::Text(text) => words.push(text.clone()),
            Argument::Input {
                input,
                prefix,
                separate,
                separator,
            } => {
                let value = bound.get(input).unwrap_or(&Json::Null);
                let texts = texts(value, &format!("inputs/{input}"), &mut inputs);
                let binding = (prefix.as_deref(), *separate, separator.as_deref());
                words.extend(bind(value, texts, binding));
            }
        }
    }
    let names = streams(spec, bound);
    let stream = |rule: CaptureRule| {
        let named = match rule {
            CaptureRule::Stderr => &names.1,
            _ => &names.0,
        };
        let captured = (spec.captures.iter()).find(|capture| capture.from == rule);
        (named.clone())
            .or_else(|| captured.map(|capture| stream_name(&names, &capture.output, &rule)))
    };
    let (stdout, stderr) = (stream(CaptureRule::Stdout), stream(CaptureRule::Stderr));
    let mut outputs: Vec<String> = stdout.iter().chain(&stderr).cloned().collect();
    let workdir = spec.workdir.as_deref().unwrap_or(WORKDIR);
    for capture in &spec.captures {
        // A pattern of a Directory output ends with `/`, so executors know what to make.
        let port = op.outputs.iter().find(|port| port.name == capture.output);
        let folders = port.is_some_and(|port| directories(&port.r#type));
        let extra = port
            .map(|port| secondaries(&port.r#type))
            .unwrap_or_default();
        for pattern in patterns(&capture.from, bound, workdir) {
            let names: Vec<String> = (extra.iter())
                .map(|item| wir::secondary_name(&pattern, &item.pattern))
                .collect();
            let pattern = match folders {
                true => format!("{pattern}/"),
                false => pattern,
            };
            for name in [pattern].into_iter().chain(names) {
                if !outputs.contains(&name) {
                    outputs.push(name);
                }
            }
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
        step: step.to_owned(),
        image: spec.image.clone(),
        command,
        script,
        workdir: Some(spec.workdir.clone().unwrap_or_else(|| WORKDIR.into())),
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
        // Without network, runs may share the results of the task.
        network: match spec.network {
            true => Network::Allowed,
            false => Network::Denied,
        },
        ..Task::default()
    }
}

/// What a task gave back: its outcome, how to read its files, and its input values.
pub struct Ended<'a> {
    pub outcome: &'a Outcome,
    pub files: Option<&'a dyn Files>,
    pub bound: &'a Map<String, Json>,
}

/// The output values that the captures of a task read from an outcome, or why the attempt
/// is not accepted: an exit code outside the success codes, or a capture that failed.
pub fn capture(
    spec: &wir::Task,
    ports: &[Port],
    ended: Ended<'_>,
    defs: &[TypeDef],
) -> Result<Map<String, Json>, String> {
    let Ended {
        outcome,
        files,
        bound,
    } = ended;
    let names = streams(spec, bound);
    let workdir = spec.workdir.as_deref().unwrap_or(WORKDIR);
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
            rule @ (CaptureRule::Glob(_) | CaptureRule::Patterns(_)) => {
                let mut found: Vec<&OutputFile> = Vec::new();
                for pattern in patterns(rule, bound, workdir) {
                    if !inside(&pattern) {
                        return Err(format!("output {}: the pattern {pattern:?}", port.name));
                    }
                    let mut matched: Vec<&OutputFile> = (outcome.files.iter())
                        .filter(|file| glob_matches(&pattern, &file.path))
                        .filter(|file| !found.iter().any(|known| known.path == file.path))
                        .collect();
                    matched.sort_by(|a, b| a.path.cmp(&b.path));
                    found.extend(matched);
                }
                found
            }
            rule => {
                let name = stream_name(&names, &port.name, rule);
                outcome
                    .files
                    .iter()
                    .filter(|file| file.path == name)
                    .collect()
            }
        };
        let mut value = fill(&port.r#type, &found, defs)
            .and_then(|mut value| {
                attach(&mut value, &port.r#type, outcome, defs)?;
                Ok(value)
            })
            .map_err(|problem| format!("output {}: {problem}", port.name))?;
        if capture.load_contents {
            load(&mut value, files)
                .map_err(|problem| format!("output {}: {problem}", port.name))?;
        }
        if !wir::fits(&value, &port.r#type, defs) {
            return Err(format!("output {} does not fit its type", port.name));
        }
        values.insert(port.name.clone(), value);
    }
    Ok(values)
}

/// Reads the text of each File of a value into its `contents`, up to the 64 KiB of CWL.
fn load(value: &mut Json, files: Option<&dyn Files>) -> Result<(), String> {
    match value {
        Json::Array(items) => items.iter_mut().try_for_each(|item| load(item, files)),
        Json::Object(file) if file.get("class") == Some(&json!("File")) => {
            let location = file
                .get("location")
                .and_then(Json::as_str)
                .unwrap_or_default();
            let bytes = workflow_graph::files::read(files, location)
                .map_err(|error| format!("cannot read {location}: {error}"))?;
            if bytes.len() > CONTENTS_LIMIT {
                return Err(format!("{location} is larger than {CONTENTS_LIMIT} bytes"));
            }
            let text = String::from_utf8_lossy(&bytes).into_owned();
            file.insert("contents".into(), Json::String(text));
            Ok(())
        }
        _ => Ok(()),
    }
}

/// The value of an output of `r#type` from the files its capture found.
fn fill(r#type: &Type, found: &[&OutputFile], defs: &[TypeDef]) -> Result<Json, String> {
    match resolve(r#type, defs) {
        Some(Type::Array(array)) => {
            let format = declared_format(&array.items, defs);
            Ok(Json::Array(
                found.iter().map(|file| entry(file, format)).collect(),
            ))
        }
        Some(Type::Optional(_)) if found.is_empty() => Ok(Json::Null),
        Some(Type::Optional(inner)) => fill(inner, found, defs),
        _ => match found {
            [one] => Ok(entry(one, declared_format(r#type, defs))),
            [] => Err("no file matched".into()),
            _ => Err(format!("{} files matched one value", found.len())),
        },
    }
}

/// The format that a File type declares, also through an optional type.
fn declared_format<'a>(r#type: &'a Type, defs: &'a [TypeDef]) -> Option<&'a str> {
    match resolve(r#type, defs)? {
        Type::File(file) => file.format.as_deref(),
        Type::Optional(inner) => declared_format(inner, defs),
        _ => None,
    }
}

/// A File or Directory value of a file that a task made, with the format that its type
/// declares.
fn entry(file: &OutputFile, format: Option<&str>) -> Json {
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
    if let (false, Some(format)) = (file.directory, format) {
        value["format"] = json!(format);
    }
    value
}

/// The arguments of an input value from its texts, after its prefix or joined, as CWL binds
/// a value with a prefix or an item separator.
fn bind(
    value: &Json,
    texts: Vec<String>,
    (prefix, separate, separator): (Option<&str>, bool, Option<&str>),
) -> Vec<String> {
    let prefixed = |text: String| match (prefix, separate) {
        (Some(prefix), true) => vec![prefix.to_owned(), text],
        (Some(prefix), false) => vec![format!("{prefix}{text}")],
        (None, _) => vec![text],
    };
    match (value, prefix, separator) {
        (_, None, None) => texts,
        (Json::Null | Json::Bool(false), _, _) => Vec::new(),
        (Json::Bool(true), Some(prefix), _) => vec![prefix.to_owned()],
        (Json::Array(items), _, _) if items.is_empty() => Vec::new(),
        (Json::Array(_), _, Some(separator)) => prefixed(texts.join(separator)),
        (Json::Array(_), Some(prefix), None) => {
            [prefix.to_owned()].into_iter().chain(texts).collect()
        }
        _ => texts.into_iter().flat_map(prefixed).collect(),
    }
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

/// The names of the stdout and stderr files that a task names, from its input values.
fn streams(spec: &wir::Task, bound: &Map<String, Json>) -> (Option<String>, Option<String>) {
    let name = |named: &Option<FileName>| match named.as_ref()? {
        FileName::Text(text) => Some(text.clone()),
        FileName::Input { input } => match bound.get(input) {
            Some(Json::String(text)) if !text.is_empty() => Some(text.clone()),
            _ => None,
        },
    };
    (name(&spec.stdout), name(&spec.stderr))
}

/// The patterns of a glob capture, also from the value of a task input, relative to the
/// working folder, which a pattern may name, as in CWL.
fn patterns(rule: &CaptureRule, bound: &Map<String, Json>, workdir: &str) -> Vec<String> {
    let texts = match rule {
        CaptureRule::Glob(pattern) => vec![pattern.clone()],
        CaptureRule::Patterns(input) => match bound.get(input) {
            Some(Json::String(text)) => vec![text.clone()],
            Some(Json::Array(items)) => (items.iter())
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    let inner = format!("{}/", workdir.trim_end_matches('/'));
    (texts.into_iter())
        .filter(|text| !text.is_empty())
        .map(|text| match text.strip_prefix(&inner) {
            Some(rest) => rest.to_owned(),
            None => text.strip_prefix("./").map_or(text.clone(), str::to_owned),
        })
        .collect()
}

/// The secondary files that the File type of an output asks for, also inside an optional, an
/// array or a union.
fn secondaries(r#type: &Type) -> Vec<wir::SecondaryFile> {
    match r#type {
        Type::File(file) => file.secondary_files.clone(),
        Type::Optional(inner) => secondaries(inner),
        Type::Array(array) => secondaries(&array.items),
        Type::Union(members) => members.iter().flat_map(secondaries).take(1).collect(),
        _ => Vec::new(),
    }
}

/// Adds to each File of an output the secondary files that its type asks for, from the files
/// beside it that the task made; a required one that is missing fails the output.
fn attach(
    value: &mut Json,
    r#type: &Type,
    outcome: &Outcome,
    defs: &[TypeDef],
) -> Result<(), String> {
    match (value, resolve(r#type, defs)) {
        (Json::Array(items), Some(Type::Array(array))) => {
            (items.iter_mut()).try_for_each(|item| attach(item, &array.items, outcome, defs))
        }
        (value, Some(Type::Optional(inner))) => attach(value, inner, outcome, defs),
        (Json::Object(file), Some(r#type @ (Type::File(_) | Type::Union(_)))) => {
            let wanted = secondaries(r#type);
            let location = file.get("location").and_then(Json::as_str);
            let made = (outcome.files.iter()).find(|item| Some(item.location.as_str()) == location);
            let Some(primary) = made.filter(|_| !wanted.is_empty()) else {
                return Ok(());
            };
            let mut found = Vec::new();
            for item in &wanted {
                let name = wir::secondary_name(&primary.path, &item.pattern);
                match outcome.files.iter().find(|file| file.path == name) {
                    Some(file) => found.push(entry(file, None)),
                    None if item.required => {
                        return Err(format!("the secondary file {name} is missing"));
                    }
                    None => {}
                }
            }
            file.insert("secondary_files".into(), Json::Array(found));
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Whether a type is a Directory, also inside an optional or an array.
fn directories(r#type: &Type) -> bool {
    match r#type {
        Type::Directory => true,
        Type::Optional(inner) => directories(inner),
        Type::Array(array) => directories(&array.items),
        _ => false,
    }
}

/// Whether a pattern is relative and stays inside the working folder.
fn inside(pattern: &str) -> bool {
    !pattern.is_empty() && !pattern.starts_with('/') && !pattern.split('/').any(|part| part == "..")
}

/// The file that a stream capture of an output reads: the one the task names, else one
/// named after the output.
fn stream_name(
    names: &(Option<String>, Option<String>),
    output: &str,
    rule: &CaptureRule,
) -> String {
    match (rule, &names.0, &names.1) {
        (CaptureRule::Stderr, _, Some(name)) => name.clone(),
        (CaptureRule::Stderr, _, None) => format!("{output}.stderr"),
        (_, Some(name), _) => name.clone(),
        _ => format!("{output}.stdout"),
    }
}

/// A word quoted for `/bin/sh`.
fn quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn binds_prefixed_values() {
        let words = |value: Json, binding| {
            let texts = texts(&value, "inputs/x", &mut Vec::new());
            bind(&value, texts, binding)
        };
        let flag = (Some("-v"), true, None);
        assert_eq!(words(json!(true), flag), ["-v"]);
        assert!(words(json!(false), flag).is_empty(), "false gives nothing");
        assert!(words(Json::Null, flag).is_empty());
        assert_eq!(words(json!(3), (Some("-n"), false, None)), ["-n3"]);
        assert_eq!(words(json!(["a", "b"]), flag), ["-v", "a", "b"]);
        assert_eq!(
            words(json!([1, 2]), (Some("-l"), true, Some(","))),
            ["-l", "1,2"]
        );
        assert!(
            words(json!([]), flag).is_empty(),
            "an empty array gives nothing"
        );
        assert_eq!(words(json!(true), (None, true, None)), ["true"]);
    }

    #[test]
    fn attaches_secondary_files() {
        let made = |path: &str| OutputFile {
            path: path.into(),
            location: format!("/out/{path}"),
            directory: false,
            size: None,
            digest: None,
        };
        let outcome = Outcome {
            files: vec![made("a.bam"), made("a.bam.bai")],
            ..Outcome::default()
        };
        let wanted: Type = serde_json::from_value(json!({"file": {"secondary_files": [
            {"pattern": ".bai"}, {"pattern": "^.txt", "required": false}
        ]}}))
        .unwrap();
        let mut value = json!({"class": "File", "location": "/out/a.bam", "basename": "a.bam"});
        attach(&mut value, &wanted, &outcome, &[]).unwrap();
        assert_eq!(value["secondary_files"][0]["basename"], "a.bam.bai");
        assert_eq!(value["secondary_files"].as_array().unwrap().len(), 1);
        let required: Type =
            serde_json::from_value(json!({"file": {"secondary_files": [{"pattern": ".csi"}]}}))
                .unwrap();
        let missing = attach(&mut value, &required, &outcome, &[]).unwrap_err();
        assert!(missing.contains("a.bam.csi"), "{missing}");
    }
}
