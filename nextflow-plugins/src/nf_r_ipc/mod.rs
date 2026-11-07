//! nf-r-ipc 0.2.2: calls R functions in a container, with arguments and results as one Arrow
//! IPC table each way, and the helpers for R's missing values, dates and durations.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod graph;
mod missing;
mod runtime;
mod table;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use executor::process::Mount;
use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::value::Value;
use sha2::{Digest, Sha256};

use crate::Context;
use crate::catalogue::{Kind, Plugin, State};
use crate::nf_python::container::{self, Call, Ended};
use crate::nf_sqldb::{split_options, thrown};
use runtime::{Settings, runtime};
use table::{CONTROL, CONTROL_FIELDS, Row};

pub(crate) const PLUGIN: Plugin = Plugin {
    functions: &[
        "rFunction",
        "rTable",
        "rRecords",
        "channelFromR",
        "isNULL",
        "isNA",
        "isNALogical",
        "isNAInteger",
        "isNADouble",
        "isNACharacter",
        "isMissing",
        "naType",
        "coalesce",
        "coalesceNULL",
        "coalesceNA",
        "assertNotMissing",
        "renderValue",
        "asLocalDate",
        "asInstantUtc",
        "asZonedDateTime",
        "asDurationSeconds",
    ],
    call: Some(call),
    script: Some(SCRIPT),
    scopes: &["nfR"],
    ..Plugin::new("nf-r-ipc", Kind::Functions, State::Partial)
};

/// `channelFromR`, a channel of the rows of `rRecords`.
const SCRIPT: &str = "include { rRecords } from 'plugin/nf-r-ipc'\n\
    def channelFromR(args, code = '') {\n    return channel.fromList(rRecords(args, code))\n}\n";

/// uwe's R launcher, which reads the request, calls the function and writes the answer.
const LAUNCHER: &str = include_str!("launcher.R");

fn call(name: &str, arguments: Vec<Value>, context: &mut Context) -> Result<Value, Error> {
    match name {
        "rFunction" => r_function(arguments, context, None),
        "rTable" => {
            let table = Value::string("table");
            let answer = r_function(arguments, context, Some(table))?;
            records(&decoded(&answer))
        }
        "rRecords" => {
            let answer = r_function(arguments, context, None)?;
            records(&decoded(&answer))
        }
        name => missing::helper(name, &arguments).unwrap_or_else(|| {
            Err(Error::Unsupported {
                message: format!("the function `{name}` of the plugin `nf-r-ipc`"),
                span: Default::default(),
            })
        }),
    }
}

/// The `decoded_data` of an answer.
fn decoded(answer: &Value) -> Value {
    let entries = crate::nf_schema::entries(answer).into_iter();
    let mut found = entries.filter(|(key, _)| key == "decoded_data");
    found.next().map_or(Value::Null, |(_, value)| value)
}

/// `rFunction([args], code)`: the answer envelope; `payload` overrides the payload kind.
fn r_function(
    arguments: Vec<Value>,
    context: &mut Context,
    payload: Option<Value>,
) -> Result<Value, Error> {
    let (mut options, positional) = split_options(arguments);
    if let Some(payload) = payload {
        options.retain(|(key, _)| key != "_payload_kind");
        options.push(("_payload_kind".to_owned(), payload));
    }
    let code = (positional.into_iter().next())
        .filter(|code| !matches!(code, Value::Null))
        .map(|code| code.to_string())
        .unwrap_or_default();
    let option =
        |name: &str| crate::nf_sqldb::option(&options, name).map(|value| value.to_string());
    let function = option("function").unwrap_or("main".to_owned());
    let script = option("script").filter(|script| !script.is_empty());
    match (&script, code.is_empty()) {
        (Some(_), false) => return Err(thrown("Cannot use both code and script options together")),
        (None, true) => return Err(thrown("Missing script or code argument")),
        _ => {}
    }
    let settings = Settings::of(&options, context);
    let runtime = runtime(&settings)?;
    let payload = settings
        .text("payload_kind")
        .unwrap_or("value_graph".to_owned());
    if payload != "value_graph" && payload != "table" {
        return Err(thrown(format!(
            "Invalid _payload_kind value: {payload}. Expected 'value_graph' or 'table'"
        )));
    }
    if !settings.list("docker_args").is_empty() {
        tracing::warn!(
            "uwe runs nf-r-ipc calls itself and ignores the docker arguments of the call"
        );
    }
    let forwarded: Vec<(String, Value)> = (options.iter())
        .filter(|(key, _)| key != "function" && key != "script" && !key.starts_with('_'))
        .cloned()
        .collect();
    let mut rows = graph::encode(&forwarded).map_err(thrown)?;
    let launch = PathBuf::from(context.host.launch_dir());
    let base = crate::nf_python::work_dir(context, &launch).join(".nf-r-ipc");
    let scratch = base.join(format!("nfr-{}", unique()));
    let failed =
        |error: std::io::Error| thrown(format!("cannot prepare {}: {error}", scratch.display()));
    std::fs::create_dir_all(&scratch).map_err(failed)?;
    let id = call_id();
    let text = |path: &Path| path.to_string_lossy().into_owned();
    let mut environment = vec![
        (
            "NFR_REQUEST_IPC".to_owned(),
            text(&scratch.join("request.arrows")),
        ),
        (
            "NFR_RESPONSE_IPC".to_owned(),
            text(&scratch.join("response.arrows")),
        ),
        ("NFR_LIBRARY".to_owned(), text(&base.join("library"))),
    ];
    let (mode, reference) = match &script {
        Some(script) => {
            let source = launch.join(script);
            let copy = scratch.join("nfr_user_script.R");
            if !source.is_file() {
                let message = match runtime.name.as_str() {
                    "docker" => format!("Docker runtime script path not found on host: {script}"),
                    _ => format!("Script path not found: {script}"),
                };
                return Err(thrown(message));
            }
            std::fs::copy(&source, &copy).map_err(failed)?;
            ("path", text(&copy))
        }
        None => {
            let file = scratch.join("nfr_inline_code.R");
            std::fs::write(&file, &code).map_err(failed)?;
            environment.push(("NFR_INLINE_CODE_FILE".to_owned(), text(&file)));
            ("inline", "<inline>".to_owned())
        }
    };
    if let Some(libraries) = settings.text("r_libs") {
        environment.push(("R_LIBS".to_owned(), libraries));
    }
    let mut control = Row {
        section: Some(CONTROL.to_owned()),
        protocol: Some(1),
        ..Row::default()
    };
    for (slot, value) in [id.as_str(), &function, mode, &reference, &payload]
        .into_iter()
        .enumerate()
    {
        control.control[slot] = Some(value.to_owned());
    }
    rows.insert(0, control);
    table::write(&rows, &scratch.join("request.arrows")).map_err(thrown)?;
    let launcher = scratch.join("nfr_launcher.R");
    std::fs::write(&launcher, LAUNCHER).map_err(failed)?;
    let mounts = (settings.list("docker_mounts").iter())
        .filter_map(|mount| {
            let mut parts = mount.split(':');
            let (source, target) = (parts.next()?, parts.next()?);
            Some(Mount {
                source: source.into(),
                target: target.to_owned(),
                read_only: parts.next() == Some("ro"),
            })
        })
        .collect();
    let output = scratch.join("launcher.log");
    let ended = container::run(&Call {
        engine: std::env::var("UWE_ENGINE").unwrap_or_default(),
        image: runtime.image.clone(),
        program: runtime.program.clone(),
        arguments: vec![text(&launcher)],
        folders: vec![base.clone()],
        mounts,
        workdir: scratch.clone(),
        environment,
        stdout: output.clone(),
        stderr: output.clone(),
        status: scratch.join(".exitcode"),
    })
    .map_err(|message| thrown(format!("nf-r-ipc could not run R: {message}")))?;
    let printed = std::fs::read_to_string(&output).unwrap_or_default();
    let tail = || {
        let lines: Vec<&str> = printed.lines().collect();
        lines[lines.len().saturating_sub(10)..].join(" | ")
    };
    let response = scratch.join("response.arrows");
    if !response.exists() {
        return Err(thrown(match ended {
            Ended::Missing => format!(
                "R launcher failed (exit=127)\nthe image has no program {}",
                runtime.program
            ),
            Ended::Exited(0) => format!(
                "R launcher did not produce response IPC (exit=0): {}\n{}",
                response.display(),
                tail()
            ),
            Ended::Exited(code) => format!("R launcher failed (exit={code})\n{printed}"),
        }));
    }
    let answer = table::read(&response).map_err(thrown)?;
    let control = (answer.iter())
        .find(|row| row.section.as_deref() == Some(CONTROL))
        .ok_or_else(|| thrown("IPC stream missing control section"))?;
    let decoded = graph::decode(&answer).map_err(thrown)?;
    let field = |name: &str| {
        let place = CONTROL_FIELDS.iter().position(|known| *known == name)?;
        control.control[place].clone()
    };
    let on_error = settings.text("on_error").unwrap_or("throw".to_owned());
    if on_error != "throw" && on_error != "return" {
        return Err(thrown(format!(
            "Invalid _on_error value: {on_error}. Expected 'throw' or 'return'"
        )));
    }
    if field("status").as_deref() == Some("error") && on_error == "throw" {
        return Err(thrown(format!(
            "rFunction failed [call_id={id}] {}: {}\nLauncher output (tail): {}",
            field("error_class").unwrap_or("RError".to_owned()),
            field("error_message").unwrap_or("unknown error".to_owned()),
            tail()
        )));
    }
    let decoded = match (decoded, payload.as_str()) {
        (Some(value), "table") => table_rows(&value).ok_or_else(|| {
            thrown(format!(
                "Invalid table payload shape. Expected list-of-records or map-of-columns, got: {}",
                value.java_class_name()
            ))
        })?,
        (None, "table") => {
            return Err(thrown(
                "Invalid table payload shape. Expected list-of-records or map-of-columns, got: null",
            ));
        }
        (decoded, _) => decoded.unwrap_or(Value::Null),
    };
    let mut shown = Vec::new();
    if let Some(protocol) = control.protocol {
        shown.push((
            "protocol_version".to_owned(),
            Value::integer(i32::try_from(protocol).unwrap_or(1)),
        ));
    }
    for name in CONTROL_FIELDS {
        if let Some(value) = field(name) {
            shown.push((name.to_owned(), Value::string(value)));
        }
    }
    let map = |entries: Vec<(String, Value)>| {
        Value::map(
            entries
                .into_iter()
                .map(|(key, value)| (Value::string(key), value)),
        )
    };
    Ok(map(vec![
        ("call_id".to_owned(), Value::string(id.as_str())),
        ("codec".to_owned(), Value::string("arrow-java")),
        ("runtime".to_owned(), map(runtime.shown)),
        ("forwarded_args".to_owned(), map(forwarded)),
        ("control".to_owned(), map(shown)),
        ("decoded_data".to_owned(), decoded),
        ("launcher_output".to_owned(), Value::string(printed)),
    ]))
}

/// The rows of a table answer: a list of maps, a map of equally long columns of scalars, or a
/// map with one entry that holds either.
fn table_rows(value: &Value) -> Option<Value> {
    match value {
        Value::List(list) => {
            let all = (list.borrow().items.iter()).all(|item| matches!(item, Value::Map(_)));
            all.then(|| value.clone())
        }
        Value::Map(map) => {
            let entries = crate::nf_schema::entries(value);
            if entries.is_empty() {
                return Some(Value::list(Vec::new()));
            }
            if graph::columns(value) {
                let length = match &entries[0].1 {
                    Value::List(list) => list.borrow().items.len(),
                    _ => 0,
                };
                let rows = (0..length).map(|place| {
                    Value::map((entries.iter()).map(|(key, column)| {
                        let cell = match column {
                            Value::List(list) => list.borrow().items[place].clone(),
                            _ => Value::Null,
                        };
                        (Value::string(key.as_str()), cell)
                    }))
                });
                return Some(Value::list(rows.collect()));
            }
            let inner = map.borrow();
            match inner.entries.values().collect::<Vec<_>>().as_slice() {
                [only] if matches!(only, Value::List(_)) || graph::columns(only) => {
                    let only = (*only).clone();
                    drop(inner);
                    table_rows(&only)
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// The rows of `rRecords`, which also unwraps a one-item list of a one-key map.
fn records(value: &Value) -> Result<Value, Error> {
    if let Some(rows) = table_rows(value) {
        return Ok(rows);
    }
    if let Value::List(list) = value {
        let items = list.borrow().items.clone();
        if let [Value::Map(map)] = items.as_slice()
            && map.borrow().entries.len() == 1
            && let Some(rows) = table_rows(&items[0])
        {
            return Ok(rows);
        }
    }
    Err(thrown(
        "rRecords expects list-of-records or map-of-columns result",
    ))
}

/// A number for the scratch folder of a call, unique on this machine.
fn unique() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = (SystemTime::now().duration_since(UNIX_EPOCH))
        .unwrap_or_default()
        .as_nanos();
    format!(
        "{nanos}{}{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// A new random UUID of version 4 for the call.
fn call_id() -> String {
    let hash = Sha256::digest(format!("{}-{:?}", unique(), std::thread::current().id()));
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&hash[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
