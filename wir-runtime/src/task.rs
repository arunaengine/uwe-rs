//! Task ops: the uwe task of a wir `task` op, and the outputs that its captures read from
//! what the task made.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;

use cwl_runtime::JavaScriptSettings;
use cwl_runtime::files::Access;

use serde_json::{Map, Value as Json, json};
use wir::{Argument, CaptureRule, Content, FileName, Op, Port, Type, TypeDef, resolve};
use workflow_graph::simulator::glob_matches;
use workflow_graph::{
    Files, Input, Network, Outcome, OutputFile, Resources, Reuse, Task, TaskId, Writable,
};

/// Where a task runs in its container: its fresh working folder.
pub const WORKDIR: &str = "/work";

/// The most bytes of a file that a capture loads as its contents, as in CWL.
const CONTENTS_LIMIT: usize = 64 * 1024;

/// The task of a task op with its input values, or why its code arguments fail. Inputs are
/// staged below `inputs/<port>`, and the attributes only set what no cache key covers.
pub fn build(
    id: TaskId,
    (path, step): (&str, &str),
    op: &Op,
    spec: &wir::Task,
    bound: &Map<String, Json>,
    (settings, simulated): (&JavaScriptSettings, bool),
) -> Result<Task, String> {
    let workdir = spec.workdir.as_deref().unwrap_or(WORKDIR);
    let mut staging = Staging::default();
    if let Some(folder) = &spec.staging {
        let names: Vec<&String> = match &spec.staged {
            None => op.inputs.iter().map(|port| &port.name).collect(),
            Some(names) => names.iter().collect(),
        };
        for name in names {
            staging.job(bound.get(name).unwrap_or(&Json::Null), folder);
        }
    }
    for staged in &spec.files {
        let value = match &staged.value {
            Content::Text(text) => Json::String(text.clone()),
            Content::Input { input } => bound.get(input).cloned().unwrap_or(Json::Null),
        };
        let path = name(&staged.path, bound);
        staging.place(&value, path.as_deref(), staged.writable, workdir);
    }
    // Each word, and whether a shell command takes it as it is.
    let mut words: Vec<(String, bool)> = Vec::new();
    for (index, argument) in spec.command.iter().enumerate() {
        match argument {
            Argument::Text(text) => words.push((text.clone(), false)),
            Argument::Raw { raw } => words.push((raw.clone(), spec.shell)),
            Argument::Input {
                input,
                prefix,
                separate,
                separator,
                quote,
            } => {
                let value = bound.get(input).unwrap_or(&Json::Null);
                let texts = staging.texts(value, &format!("inputs/{input}"));
                let binding = (prefix.as_deref(), *separate, separator.as_deref());
                let raw = spec.shell && !quote;
                words.extend(
                    bind(value, texts, binding)
                        .into_iter()
                        .map(|word| (word, raw)),
                );
            }
            Argument::Code {
                code,
                this,
                prefix,
                separate,
                separator,
                quote,
            } => {
                // As CWL binds no input that is null, its code gives no argument then.
                if this
                    .as_ref()
                    .is_some_and(|name| bound.get(name).is_none_or(Json::is_null))
                {
                    continue;
                }
                let staged = staging.adjusted(&Json::Object(bound.clone()), workdir);
                let given = this.as_ref().and_then(|name| staged.get(name)).cloned();
                let values = (staged, given.unwrap_or(Json::Null), json!({}));
                let value = match crate::expr::code(code, values, (&spec.library, settings)) {
                    Ok(value) => crate::cwl::to_wir(value),
                    // As uwe's CWL driver, a simulated run marks code over made-up contents.
                    Err(_) if simulated && code.contains("contents") => {
                        json!(cwl_runtime::tool::FAKE_ARGUMENT)
                    }
                    Err(why) => return Err(format!("argument {index}: {why}")),
                };
                let texts = staging.texts(&value, &format!("arguments/{index}"));
                let binding = (prefix.as_deref(), *separate, separator.as_deref());
                let raw = spec.shell && !quote;
                let bound = bind(&value, texts, binding).into_iter();
                words.extend(bound.map(|word| (word, raw)));
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
    for capture in &spec.captures {
        // A pattern of a Directory output ends with `/`, so executors know what to make.
        let port = op.outputs.iter().find(|port| port.name == capture.output);
        let folders = port.is_some_and(|port| directories(&port.r#type));
        let extra = port
            .map(|port| secondaries(&port.r#type))
            .unwrap_or_default();
        // A stream file has its secondary files beside it too.
        let read = match &capture.from {
            CaptureRule::Stdout => stdout.iter().cloned().collect(),
            CaptureRule::Stderr => stderr.iter().cloned().collect(),
            rule => patterns(rule, bound, workdir),
        };
        for pattern in read {
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
    // Code may name any file it made, so a real run takes them all, as uwe's CWL driver.
    let named = (spec.captures.iter()).any(|capture| {
        let port = op.outputs.iter().find(|port| port.name == capture.output);
        capture.code.is_some() && port.is_some_and(|port| holds_files(&port.r#type))
    });
    if named && !simulated && !outputs.iter().any(|output| output == "*") {
        outputs.push("*".into());
    }
    // A simulated run lists the results file only where an output needs it, as uwe's CWL driver.
    let listed = !simulated || (spec.captures.iter()).any(|item| item.from == CaptureRule::Results);
    if let Some(name) = spec
        .results
        .as_ref()
        .filter(|name| listed && !outputs.contains(name))
    {
        outputs.push(name.clone());
    }
    let stdin = match &spec.stdin {
        Some(Content::Text(path)) => Some(path.clone()),
        Some(Content::Input { input }) => {
            let value = bound.get(input).unwrap_or(&Json::Null);
            staging.texts(value, &format!("inputs/{input}")).pop()
        }
        None => None,
    };
    let mut environment = std::collections::BTreeMap::new();
    for (name, content) in &spec.environment {
        let value = match content {
            Content::Text(text) => Json::String(text.clone()),
            Content::Input { input } => bound.get(input).cloned().unwrap_or(Json::Null),
        };
        match value {
            Json::Null => {}
            Json::String(text) => drop(environment.insert(name.clone(), text)),
            other => drop(environment.insert(name.clone(), other.to_string())),
        }
    }
    let streams = [(">", &stdout), ("2>", &stderr), ("<", &stdin)];
    let redirections: Vec<String> = streams
        .into_iter()
        .filter_map(|(sign, name)| name.as_ref().map(|name| format!("{sign} {}", quote(name))))
        .collect();
    // As uwe's CWL driver builds them: a shell command or redirections make a script.
    let (command, script) = match spec.shell || !redirections.is_empty() {
        true => {
            let mut script = String::new();
            if !redirections.is_empty() {
                script.push_str(&format!("exec {}\n", redirections.join(" ")));
            }
            let quoted: Vec<String> = (words.iter())
                .map(|(word, raw)| if *raw { word.clone() } else { quote(word) })
                .collect();
            script.push_str(&quoted.join(" "));
            (
                vec!["/bin/sh".into(), "-c".into(), script.clone()],
                Some(script),
            )
        }
        false => {
            let words: Vec<String> = words.into_iter().map(|(word, _)| word).collect();
            let script = match words.as_slice() {
                [shell, flag, script, ..]
                    if flag == "-c" && matches!(shell.rsplit('/').next(), Some("sh" | "bash")) =>
                {
                    Some(script.clone())
                }
                _ => None,
            };
            (words, script)
        }
    };
    let attributes = &op.attributes;
    let wanted = attributes.resources.clone().unwrap_or_default();
    let limits = spec.limits.as_ref().and_then(|name| bound.get(name));
    let limit = |key: &str| {
        limits
            .and_then(|limits| limits.get(key))
            .and_then(Json::as_f64)
    };
    Ok(Task {
        id,
        name: path.to_owned(),
        step: step.to_owned(),
        image: spec.image.clone(),
        command,
        script,
        workdir: Some(workdir.to_owned()),
        inputs: staging.inputs,
        outputs,
        resources: Resources {
            cpus: limit("cpus").or(wanted.cpus.map(f64::from)),
            memory_bytes: limit("memory").map(|bytes| bytes as u64).or(wanted.memory),
            disk_bytes: limit("disk").map(|bytes| bytes as u64).or(wanted.disk),
            time_seconds: limit("time")
                .map(|seconds| seconds as u64)
                .or(attributes.time),
            gpus: limit("gpus").map(|gpus| gpus as u32).or(wanted.gpus),
            ..Resources::default()
        },
        environment,
        output_manifest: spec.results.clone().filter(|_| !simulated),
        reuse: match attributes.cache {
            Some(wir::Cache::Never) => Reuse::Disabled,
            Some(wir::Cache::Resume) => Reuse::Lineage,
            // The outputs or the effect of an op with effects may differ from another run's.
            _ if !op.effects.is_empty() => Reuse::Lineage,
            _ => Reuse::Allowed,
        },
        // Without network, runs may share the results of the task.
        network: match spec.network {
            true => Network::Allowed,
            false => Network::Denied,
        },
        ..Task::default()
    })
}

/// What a task gave back: its outcome, how to read its files, its input values and staged
/// inputs, whether the run is simulated, and how code reads files.
#[derive(Clone, Copy)]
pub struct Ended<'a> {
    pub outcome: &'a Outcome,
    pub files: Option<&'a dyn Files>,
    pub bound: &'a Map<String, Json>,
    pub inputs: &'a [Input],
    pub simulated: bool,
    pub settings: &'a JavaScriptSettings,
    pub access: &'a Access,
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
        inputs,
        simulated,
        ..
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
    let results = match &spec.results {
        Some(name) => self::results(name, outcome, files, simulated)?,
        None => None,
    };
    let mut values = Map::new();
    for port in ports {
        let Some(capture) = spec.captures.iter().find(|item| item.output == port.name) else {
            return Err(format!("output {} has no capture", port.name));
        };
        let given = match (&results, &capture.from) {
            (Some(object), _) => object.get(&port.name).cloned(),
            (None, CaptureRule::Results) => Some(Json::Null),
            _ => None,
        };
        if results.is_some() || given.is_some() {
            let value = located(
                given.unwrap_or(Json::Null),
                (outcome, inputs, workdir, simulated),
            )
            .map_err(|problem| format!("output {}: {problem}", port.name))?;
            let value = crate::cwl::to_wir(value);
            values.insert(port.name.clone(), settled(value, port, defs, simulated)?);
            continue;
        }
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
            CaptureRule::Results | CaptureRule::Empty => Vec::new(),
            rule => {
                let name = stream_name(&names, &port.name, rule);
                outcome
                    .files
                    .iter()
                    .filter(|file| file.path == name)
                    .collect()
            }
        };
        if let Some(code) = &capture.code {
            let value = match evaluated((code, capture), &found, spec, ended) {
                // A simulated run makes files up, so code over them may fail.
                Err(_)
                    if simulated && ["self", "contents"].iter().any(|word| code.contains(word)) =>
                {
                    crate::expr::placeholder(&port.r#type, &port.name, defs)
                }
                other => other.map_err(|problem| format!("output {}: {problem}", port.name))?,
            };
            let mut value = crate::cwl::to_wir(value);
            attach(&mut value, &port.r#type, outcome, defs)
                .map_err(|problem| format!("output {}: {problem}", port.name))?;
            values.insert(port.name.clone(), settled(value, port, defs, simulated)?);
            continue;
        }
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
        if let Some(listing) = capture.listing {
            value = crate::load::listings(value, listing, ended.access)
                .map_err(|problem| format!("output {}: {problem}", port.name))?;
        }
        if !wir::fits(&value, &port.r#type, defs) {
            return Err(format!("output {} does not fit its type", port.name));
        }
        values.insert(port.name.clone(), value);
    }
    Ok(values)
}

/// An output value from the results or code, as wir holds it; a simulated run makes up one
/// that does not fit.
fn settled(value: Json, port: &Port, defs: &[TypeDef], simulated: bool) -> Result<Json, String> {
    let value = crate::state::widen(value, &port.r#type, defs);
    match wir::fits(&value, &port.r#type, defs) {
        true => Ok(value),
        false if simulated => Ok(crate::expr::placeholder(&port.r#type, &port.name, defs)),
        false => Err(format!("output {} does not fit its type", port.name)),
    }
}

/// The value of the code of a capture over the paths it matched, as uwe's CWL driver
/// evaluates `outputEval`.
fn evaluated(
    (code, capture): (&str, &wir::Capture),
    found: &[&OutputFile],
    spec: &wir::Task,
    ended: Ended<'_>,
) -> Result<Json, String> {
    let workdir = spec.workdir.as_deref().unwrap_or(WORKDIR);
    let mut this = Json::Array(found.iter().map(|file| entry(file, None)).collect());
    if capture.load_contents {
        load(&mut this, ended.files)?;
    }
    if let Some(listing) = capture.listing {
        this = crate::load::listings(this, listing, ended.access)?;
    }
    let location = |item: &Json| {
        item.get("location")
            .and_then(Json::as_str)
            .map(str::to_owned)
    };
    let made = |item: &Json| {
        let location = location(item)?;
        let file = (ended.outcome.files.iter()).find(|file| file.location == location)?;
        Some(absolute(file.path.trim_end_matches('/'), workdir))
    };
    let given = |item: &Json| {
        let location = location(item)?;
        let source = cwl_parser::iri::file_path(&location)
            .map_or(location, |path| path.to_string_lossy().into_owned());
        let input = ended.inputs.iter().find(|input| input.source == source)?;
        Some(absolute(&input.path, workdir))
    };
    let inputs = with_paths(&Json::Object(ended.bound.clone()), &given);
    let runtime = json!({"exitCode": ended.outcome.exit_code});
    let values = (inputs, with_paths(&this, &made), runtime);
    let value = crate::expr::code(code, values, (&spec.library, ended.settings))?;
    located(
        value,
        (ended.outcome, ended.inputs, workdir, ended.simulated),
    )
}

/// The object of the results file that the task wrote, if any; the made-up file of a
/// simulated run may hold none.
fn results(
    name: &str,
    outcome: &Outcome,
    files: Option<&dyn Files>,
    simulated: bool,
) -> Result<Option<Map<String, Json>>, String> {
    let Some(file) = outcome.files.iter().find(|file| file.path == name) else {
        return Ok(None);
    };
    let bytes = workflow_graph::files::read(files, &file.location)
        .map_err(|error| format!("cannot read {name}: {error}"))?;
    match serde_json::from_slice(&bytes) {
        Ok(Json::Object(object)) => Ok(Some(object)),
        _ if simulated => Ok(None),
        Ok(_) => Err(format!("{name} holds no JSON object")),
        Err(error) => Err(format!("{name}: {error}")),
    }
}

/// A value of the results where each File and Directory that names a place in the container
/// becomes the file that the task made there, or the input staged there.
fn located(value: Json, task: (&Outcome, &[Input], &str, bool)) -> Result<Json, String> {
    let (outcome, inputs, workdir, simulated) = task;
    let mut object = match value {
        Json::Array(items) => {
            let items = items.into_iter().map(|item| located(item, task));
            return items.collect::<Result<_, _>>().map(Json::Array);
        }
        Json::Object(object) => object,
        other => return Ok(other),
    };
    for (_, item) in object.iter_mut() {
        *item = located(item.take(), task)?;
    }
    if !matches!(class(&object), Some("File" | "Directory")) {
        return Ok(Json::Object(object));
    }
    let place = (object.get("path").or(object.get("location"))).and_then(Json::as_str);
    let Some(place) = place.filter(|place| !place.starts_with("_:")) else {
        return Ok(Json::Object(object));
    };
    let path = cwl_parser::iri::file_path(place)
        .map_or(place.to_owned(), |path| path.to_string_lossy().into_owned());
    let path = absolute(path.trim_start_matches("./"), workdir);
    let made = (outcome.files.iter())
        .find(|file| absolute(file.path.trim_end_matches('/'), workdir) == path);
    let mut found = match made {
        Some(file) => entry(file, None),
        None => {
            let staged = (inputs.iter()).find(|input| absolute(&input.path, workdir) == path);
            // A place in the working folder is in the folder of the outputs, as uwe's CWL driver.
            let folder = (outcome.files.iter()).find_map(|file| {
                let made = file.path.trim_end_matches('/');
                (file.location.trim_end_matches('/').strip_suffix(made))
                    .map(|folder| folder.trim_end_matches('/').to_owned())
            });
            let location = match (staged, relative(&path, workdir), folder) {
                (Some(input), _, _) => input.source.clone(),
                (None, Some(inner), Some(folder)) => join(&folder, &inner),
                // A simulated run that made no files takes them as present.
                (None, Some(inner), None) if simulated => format!("_:{inner}"),
                _ => {
                    return Err(format!(
                        "the results name {place}, which the task did not make"
                    ));
                }
            };
            let name = path.rsplit('/').next().unwrap_or_default();
            json!({"class": class(&object), "location": location, "basename": name})
        }
    };
    for (key, item) in object {
        if !matches!(
            key.as_str(),
            "location" | "path" | "dirname" | "nameroot" | "nameext"
        ) {
            found[key] = item;
        }
    }
    Ok(found)
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

/// The inputs of a task, and the paths in the working folder of the values it places there.
#[derive(Default)]
struct Staging {
    inputs: Vec<Input>,
    placed: HashMap<String, String>,
    /// The paths of Files and Directories without a location, by their JSON text.
    literals: HashMap<String, String>,
    /// How many numbered folders the staging folder has.
    folders: usize,
}

impl Staging {
    /// The arguments of an input value, staging its files and folders below `folder`, or
    /// where the working folder holds them.
    fn texts(&mut self, value: &Json, folder: &str) -> Vec<String> {
        match value {
            Json::Null => Vec::new(),
            Json::Bool(flag) => vec![flag.to_string()],
            // Floats in positional form, as uwe's CWL driver writes them.
            Json::Number(number) => match number.as_f64().filter(|_| number.is_f64()) {
                Some(float) => vec![cwl_runtime::value::plain_float(float)],
                None => vec![number.to_string()],
            },
            Json::String(text) => vec![text.clone()],
            Json::Array(items) => (items.iter().enumerate())
                .flat_map(|(index, item)| self.texts(item, &format!("{folder}/{index}")))
                .collect(),
            Json::Object(_) => match self.stage(value, folder) {
                Some(path) => vec![path],
                None => vec![value.to_string()],
            },
        }
    }

    /// Stages a File or Directory value and its secondary files in `folder`; its path there,
    /// or in the working folder when an entry placed it there.
    fn stage(&mut self, value: &Json, folder: &str) -> Option<String> {
        let class = value.get("class").and_then(Json::as_str)?;
        if !matches!(class, "File" | "Directory") {
            return None;
        }
        let location = value.get("location").and_then(Json::as_str);
        if let Some(path) = self.path(value) {
            return Some(path.clone());
        }
        let path = format!(
            "{folder}/{}",
            basename(value).unwrap_or_else(|| "input".into())
        );
        self.put(value, &path, false);
        if location.is_none() {
            self.literals.insert(value.to_string(), path.clone());
        }
        for item in value
            .get("secondary_files")
            .and_then(Json::as_array)
            .into_iter()
            .flatten()
        {
            self.stage(item, folder);
        }
        Some(path)
    }

    /// Stages each File and Directory with a location of an input value in a numbered folder
    /// of its own below `folder`, as uwe's CWL driver stages a job.
    fn job(&mut self, value: &Json, folder: &str) {
        match value {
            Json::Array(items) => items.iter().for_each(|item| self.job(item, folder)),
            Json::Object(object) if matches!(class(object), Some("File" | "Directory")) => {
                if self.path(value).is_none() {
                    self.stage(value, &format!("{folder}/{}", self.folders));
                    self.folders += 1;
                }
            }
            Json::Object(object) => object.values().for_each(|item| self.job(item, folder)),
            _ => {}
        }
    }

    /// Where a File or Directory value is staged.
    fn path(&self, value: &Json) -> Option<&String> {
        match value.get("location").and_then(Json::as_str) {
            Some(location) => self.placed.get(location),
            None => self.literals.get(&value.to_string()),
        }
    }

    /// A value whose staged Files and Directories have their paths in the container.
    fn adjusted(&self, value: &Json, workdir: &str) -> Json {
        with_paths(value, &|item| {
            self.path(item).map(|path| absolute(path, workdir))
        })
    }

    /// Places the value of an entry of the working folder at `path`, else by basename.
    fn place(&mut self, value: &Json, path: Option<&str>, writable: bool, workdir: &str) {
        let at = |name: String| relative(&name, workdir);
        match value {
            Json::Null => {}
            Json::Array(items) if path.is_none() => {
                (items.iter()).for_each(|item| self.place(item, None, writable, workdir));
            }
            Json::Object(object) if object.contains_key("entry") => {
                let name = object.get("entryname").and_then(Json::as_str).or(path);
                let writable = writable || object.get("writable") == Some(&Json::Bool(true));
                self.place(&object["entry"], name, writable, workdir);
            }
            Json::Object(object) if matches!(class(object), Some("File" | "Directory")) => {
                let name = path.map(str::to_owned).or_else(|| basename(value));
                let Some(name) = name.and_then(at) else {
                    return;
                };
                self.put(value, &name, writable);
                let folder = name.rsplit_once('/').map_or("", |(folder, _)| folder);
                for item in object
                    .get("secondary_files")
                    .and_then(Json::as_array)
                    .into_iter()
                    .flatten()
                {
                    if let Some(beside) = basename(item).map(|base| join(folder, &base)) {
                        self.put(item, &beside, writable);
                    }
                }
            }
            other => {
                // Other values are JSON text as Python writes it, as in uwe's CWL driver.
                let python = |value: &Json| {
                    let value =
                        cwl_runtime::Value::parse(&crate::cwl::to_cwl(value.clone()).to_string());
                    value.map_or_else(|_| other.to_string(), |value| value.to_python_json(true))
                };
                let text = match other {
                    Json::String(text) => text.clone(),
                    other => python(other),
                };
                if let Some(name) = path.map(str::to_owned).and_then(at) {
                    self.inputs.push(Input {
                        path: name,
                        content: Some(text),
                        writable: copy(writable),
                        ..Input::default()
                    });
                }
            }
        }
    }

    /// Stages a File or Directory at `path`; a Directory without a location is made, with its
    /// listing in it.
    fn put(&mut self, value: &Json, path: &str, writable: bool) {
        let directory = value.get("class").and_then(Json::as_str) == Some("Directory");
        let location = value.get("location").and_then(Json::as_str);
        let source = match location {
            Some(text) => cwl_parser::iri::file_path(text)
                .map_or(text.to_owned(), |path| path.to_string_lossy().into_owned()),
            None => String::new(),
        };
        let content = (value.get("contents").and_then(Json::as_str)).filter(|_| location.is_none());
        self.inputs.push(Input {
            path: path.to_owned(),
            source,
            directory,
            content: content.map(str::to_owned),
            writable: copy(writable),
            ..Input::default()
        });
        if let Some(location) = location {
            self.placed.insert(location.to_owned(), path.to_owned());
        }
        if directory && location.is_none() {
            let listing = value.get("listing").and_then(Json::as_array);
            for item in listing.into_iter().flatten() {
                if let Some(name) = basename(item) {
                    self.put(item, &join(path, &name), writable);
                }
                let beside = item.get("secondary_files").and_then(Json::as_array);
                for secondary in beside.into_iter().flatten() {
                    if let Some(name) = basename(secondary) {
                        self.put(secondary, &join(path, &name), writable);
                    }
                }
            }
        }
    }
}

fn class(object: &Map<String, Json>) -> Option<&str> {
    object.get("class").and_then(Json::as_str)
}

/// The basename of a File or Directory, else the last part of its location, decoded.
fn basename(value: &Json) -> Option<String> {
    if let Some(name) = value.get("basename").and_then(Json::as_str) {
        return Some(name.to_owned());
    }
    let location = value.get("location").and_then(Json::as_str)?;
    let path = cwl_parser::iri::file_path(location).map(|path| path.to_string_lossy().into_owned());
    let path = path.unwrap_or_else(|| location.to_owned());
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .map(str::to_owned)
}

fn join(folder: &str, name: &str) -> String {
    match folder.is_empty() {
        true => name.to_owned(),
        false => format!("{folder}/{name}"),
    }
}

/// A value whose Files and Directories have the `path`, `dirname` and `basename` that
/// `path_of` gives them, as uwe's CWL driver adjusts them.
fn with_paths(value: &Json, path_of: &dyn Fn(&Json) -> Option<String>) -> Json {
    match value {
        Json::Array(items) => {
            Json::Array(items.iter().map(|item| with_paths(item, path_of)).collect())
        }
        Json::Object(object) => {
            let path = path_of(value);
            let mut object: Map<String, Json> = (object.iter())
                .map(|(key, item)| (key.clone(), with_paths(item, path_of)))
                .collect();
            if let Some(path) = path {
                let (folder, name) = path.rsplit_once('/').unwrap_or(("", &path));
                object.insert("dirname".into(), json!(folder));
                object.insert("basename".into(), json!(name));
                object.insert("path".into(), json!(path));
            }
            Json::Object(object)
        }
        other => other.clone(),
    }
}

/// A path in the container, from the working folder when it is relative.
fn absolute(path: &str, workdir: &str) -> String {
    match path.starts_with('/') {
        true => path.to_owned(),
        false => join(workdir, path),
    }
}

fn copy(writable: bool) -> Writable {
    match writable {
        true => Writable::Copy,
        false => Writable::No,
    }
}

/// A path of the working folder relative to it, also when it names the folder, or none when
/// it leaves the folder.
fn relative(path: &str, workdir: &str) -> Option<String> {
    let inner = format!("{}/", workdir.trim_end_matches('/'));
    let path = path.strip_prefix(&inner).unwrap_or(path);
    let path = path.strip_prefix("./").unwrap_or(path);
    inside(path).then(|| path.to_owned())
}

/// The name that a stream or an entry names: text, or the string value of a task input.
fn name(named: &Option<FileName>, bound: &Map<String, Json>) -> Option<String> {
    match named.as_ref()? {
        FileName::Text(text) => Some(text.clone()),
        FileName::Input { input } => match bound.get(input) {
            Some(Json::String(text)) if !text.is_empty() => Some(text.clone()),
            _ => None,
        },
    }
}

/// The names of the stdout and stderr files that a task names, from its input values.
fn streams(spec: &wir::Task, bound: &Map<String, Json>) -> (Option<String>, Option<String>) {
    (name(&spec.stdout, bound), name(&spec.stderr, bound))
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

/// Whether a type may hold Files or Directories; a named type counts.
fn holds_files(r#type: &Type) -> bool {
    match r#type {
        Type::File(_) | Type::Directory | Type::Dynamic | Type::Named(_) => true,
        Type::Optional(inner) => holds_files(inner),
        Type::Array(array) => holds_files(&array.items),
        Type::Union(members) | Type::Tuple(members) => members.iter().any(holds_files),
        Type::Record(fields) => fields.iter().any(|field| holds_files(&field.r#type)),
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

/// A word quoted for `/bin/sh` where it needs it, as uwe's CWL driver quotes.
fn quote(word: &str) -> String {
    cwl_runtime::binding::shell_quote(word)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn binds_prefixed_values() {
        let words = |value: Json, binding| {
            let texts = Staging::default().texts(&value, "inputs/x");
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
    fn places_working_files() {
        let mut staging = Staging::default();
        let genome = json!({"class": "File", "location": "/data/g.fa", "basename": "g.fa"});
        staging.place(&genome, None, true, "/var/spool/cwl");
        staging.place(&json!("x=1"), Some("conf/x.conf"), false, "/var/spool/cwl");
        let folder = json!({"class": "Directory", "basename": "d", "listing": [genome.clone()]});
        staging.place(&folder, Some("/var/spool/cwl/out"), false, "/var/spool/cwl");
        staging.place(&json!("no"), Some("../x"), false, "/var/spool/cwl");
        let paths: Vec<&str> = staging
            .inputs
            .iter()
            .map(|input| input.path.as_str())
            .collect();
        assert_eq!(paths, ["g.fa", "conf/x.conf", "out", "out/g.fa"]);
        assert_eq!(staging.inputs[0].writable, Writable::Copy);
        assert_eq!(staging.inputs[1].content.as_deref(), Some("x=1"));
        assert!(staging.inputs[2].directory && staging.inputs[2].source.is_empty());
        let words = staging.texts(&genome, "inputs/genome");
        assert_eq!(words, ["out/g.fa"], "the last place of a file is its path");
    }

    #[test]
    fn reads_results_file() {
        let folder = tempfile::tempdir().unwrap();
        let written = folder.path().join("cwl.output.json");
        let object = json!({"made": {"class": "File", "path": "/w/a.txt"},
            "given": {"class": "File", "location": "in/b.txt"}, "count": 3});
        std::fs::write(&written, object.to_string()).unwrap();
        let file = |path: &str, location: &str| OutputFile {
            path: path.into(),
            location: location.into(),
            directory: false,
            size: None,
            digest: None,
        };
        let outcome = Outcome {
            files: vec![
                file("a.txt", "/out/a.txt"),
                file("cwl.output.json", &written.to_string_lossy()),
            ],
            ..Outcome::default()
        };
        let rule = |output: &str, from| wir::Capture {
            output: output.into(),
            from,
            load_contents: false,
            optional: false,
            arity: None,
            listing: None,
            code: None,
        };
        let spec = wir::Task {
            workdir: Some("/w".into()),
            results: Some("cwl.output.json".into()),
            captures: vec![
                rule("made", CaptureRule::Glob("none".into())),
                rule("given", CaptureRule::Results),
                rule("count", CaptureRule::Results),
            ],
            ..wir::Task::default()
        };
        let port = |name: &str, r#type: Type| Port {
            name: name.into(),
            r#type,
            flow: wir::Flow::Value,
            default: None,
        };
        let text = Type::File(wir::FileType::default());
        let ports = [
            port("made", text.clone()),
            port("given", text),
            port("count", Type::Int),
        ];
        let inputs = [Input {
            path: "in/b.txt".into(),
            source: "/data/b.txt".into(),
            ..Input::default()
        }];
        let ended = Ended {
            outcome: &outcome,
            files: None,
            bound: &Map::new(),
            inputs: &inputs,
            simulated: false,
            settings: &JavaScriptSettings::default(),
            access: &Access::default(),
        };
        let values = capture(&spec, &ports, ended, &[]).unwrap();
        assert_eq!(
            values["made"]["location"], "/out/a.txt",
            "the file the task made"
        );
        assert_eq!(
            values["given"]["location"], "/data/b.txt",
            "the staged input"
        );
        assert_eq!(values["given"]["basename"], "b.txt");
        assert_eq!(values["count"], 3);
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
