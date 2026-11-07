//! The `bco` report of nf-prov: a BioCompute Object (IEEE 2791) of the run, with an etag that
//! is the SHA-256 of its content.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt::Write as _;
use std::io::Read as _;

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{Run, media_type, normalise, task_hash};
use crate::report_format::{Json, NEXTFLOW_VERSION, offset_time};

const SPEC: &str = "https://w3id.org/ieee/ieee-2791-schema/2791object.json";
const SCM: &str = "https://w3id.org/biocompute/extension_domain/1.1.0/scm/scm_extension.json";

/// The BioCompute Object as JSON text, with the options of `prov.formats.bco`.
pub(super) fn render(run: &Run, options: &Value) -> String {
    let content = Json::object([
        ("provenance_domain", provenance(run, options)),
        (
            "usability_domain",
            option(options, "/usability_domain", Json::List(Vec::new())),
        ),
        ("extension_domain", extension(run)),
        ("description_domain", description(run, options)),
        ("execution_domain", execution(run, options)),
        ("parametric_domain", parametric(run)),
        ("io_domain", io(run)),
        (
            "error_domain",
            Json::object([
                ("empirical_error", Json::Object(Vec::new())),
                ("algorithmic_error", Json::Object(Vec::new())),
            ]),
        ),
    ])
    .without_nulls();
    let etag = Sha256::digest(sorted(&content).compact().as_bytes());
    let mut object = Json::object([
        ("object_id", Json::from(format!("urn:uuid:{}", uuid()))),
        ("spec_version", SPEC.into()),
        ("etag", hex(&etag).into()),
    ]);
    if let Json::Object(entries) = content {
        for (key, value) in entries {
            object.insert(&key, value);
        }
    }
    object.pretty()
}

fn provenance(run: &Run, options: &Value) -> Json {
    let manifest = &run.start.manifest;
    let text = |key: &str| Json::from(manifest[key].as_str().unwrap_or_default());
    let created = Json::from(offset_time(run.start.start));
    let contributors = (manifest["contributors"].as_array().into_iter().flatten()).map(|person| {
        let roles = (person["contribution"].as_array().into_iter().flatten())
            .filter_map(Value::as_str)
            .map(|role| match role {
                "author" => "authoredBy",
                _ => "curatedBy",
            });
        Json::object([
            ("name", Json::from(&person["name"])),
            ("affiliation", Json::from(&person["affiliation"])),
            ("email", Json::from(&person["email"])),
            ("contribution", Json::list(roles)),
            ("orcid", Json::from(&person["orcid"])),
        ])
    });
    Json::object([
        ("name", text("name")),
        ("version", text("version")),
        (
            "review",
            option(options, "/provenance_domain/review", Json::List(Vec::new())),
        ),
        (
            "derived_from",
            option(options, "/provenance_domain/derived_from", Json::Null),
        ),
        (
            "obsolete_after",
            option(options, "/provenance_domain/obsolete_after", Json::Null),
        ),
        (
            "embargo",
            option(options, "/provenance_domain/embargo", Json::Null),
        ),
        ("created", created.clone()),
        ("modified", created),
        ("contributors", Json::list(contributors)),
        ("license", text("license")),
    ])
}

fn extension(run: &Run) -> Json {
    let start = run.start;
    let Some(repository) = &start.repository else {
        return Json::List(Vec::new());
    };
    let script = (start.script.strip_prefix(&start.project_dir))
        .map(|path| path.trim_start_matches('/'))
        .unwrap_or(&start.script);
    Json::list([Json::object([
        ("extension_schema", Json::from(SCM)),
        (
            "scm_extension",
            Json::object([
                ("scm_repository", Json::from(repository)),
                ("scm_type", "git".into()),
                ("scm_commit", start.commit_id.as_deref().into()),
                ("scm_path", script.into()),
                ("scm_preview", normalise(&start.script).into()),
            ]),
        ),
    ])])
}

fn description(run: &Run, options: &Value) -> Json {
    let mut tasks: Vec<_> = run.tasks.iter().collect();
    tasks.sort_by(|a, b| {
        let name = |task: &crate::observer::Task| task.record["name"].as_str().map(str::to_owned);
        (run.step(a), name(a), task_hash(a)).cmp(&(run.step(b), name(b), task_hash(b)))
    });
    let steps = tasks.into_iter().map(|task| {
        let uris = |files: &[crate::observer::File]| {
            Json::list(
                (files.iter()).map(|file| Json::object([("uri", normalise(&file.path).into())])),
            )
        };
        Json::object([
            ("step_number", Json::from(u64::from(run.step(task)))),
            ("name", task_hash(task).into()),
            ("description", Json::from(&task.record["name"])),
            ("input_list", uris(&task.inputs)),
            ("output_list", uris(&task.outputs)),
        ])
    });
    Json::object([
        (
            "keywords",
            option(
                options,
                "/description_domain/keywords",
                Json::List(Vec::new()),
            ),
        ),
        (
            "xref",
            option(options, "/description_domain/xref", Json::List(Vec::new())),
        ),
        ("platform", Json::list(["Nextflow"])),
        ("pipeline_steps", Json::list(steps)),
    ])
}

fn execution(run: &Run, options: &Value) -> Json {
    let script = Json::object([("uri", normalise(&run.start.script).into())]);
    let release =
        format!("https://github.com/nextflow-io/nextflow/releases/tag/v{NEXTFLOW_VERSION}");
    let names = options.pointer("/execution_domain/environment_variables");
    let variables = (names.and_then(Value::as_array).into_iter().flatten())
        .filter_map(Value::as_str)
        .filter_map(|name| Some((name, Json::from(std::env::var(name).ok()?))));
    Json::object([
        ("script", Json::list([Json::object([("uri", script)])])),
        ("script_driver", "nextflow".into()),
        (
            "software_prerequisites",
            Json::list([Json::object([
                ("name", Json::from("Nextflow")),
                ("version", NEXTFLOW_VERSION.into()),
                ("uri", Json::object([("uri", release.into())])),
            ])]),
        ),
        (
            "external_data_endpoints",
            option(
                options,
                "/execution_domain/external_data_endpoints",
                Json::List(Vec::new()),
            ),
        ),
        ("environment_variables", Json::object(variables)),
    ])
}

/// One entry per param, nested maps as dotted names, with the steps of the tasks that staged
/// the param's path or a path below it.
fn parametric(run: &Run) -> Json {
    let mut params = Vec::new();
    flatten("", &run.start.params, &mut params);
    let entries = params.into_iter().map(|(name, value)| {
        let text = match value {
            Value::String(text) => normalise(text),
            other => other.to_string(),
        };
        let mut steps: Vec<u32> = match value.as_str() {
            Some(path) if !path.is_empty() => (run.tasks.iter())
                .filter(|task| {
                    (task.inputs.iter()).any(|input| {
                        input.path == path || input.path.starts_with(&format!("{path}/"))
                    })
                })
                .map(|task| run.step(task))
                .collect(),
            _ => Vec::new(),
        };
        steps.sort_unstable();
        steps.dedup();
        let steps: Vec<String> = steps.iter().map(u32::to_string).collect();
        Json::object([
            ("param", Json::from(name)),
            ("value", text.into()),
            ("step", steps.join(",").into()),
        ])
    });
    Json::list(entries)
}

fn flatten<'a>(prefix: &str, value: &'a Value, into: &mut Vec<(String, &'a Value)>) {
    let Value::Object(entries) = value else {
        return;
    };
    for (key, value) in entries {
        let name = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match value {
            Value::Object(_) => flatten(&name, value, into),
            Value::Null => {}
            other => into.push((name, other)),
        }
    }
}

fn io(run: &Run) -> Json {
    let mut inputs: Vec<String> = run.inputs().iter().map(|path| normalise(path)).collect();
    inputs.sort();
    let inputs = inputs
        .into_iter()
        .map(|uri| Json::object([("uri", Json::object([("uri", uri.into())]))]));
    let mut outputs: Vec<(String, &str, &str)> = (run.published.iter())
        .map(|(target, source)| (normalise(target), target.as_str(), source.as_str()))
        .collect();
    outputs.sort();
    let outputs = outputs.into_iter().map(|(uri, target, source)| {
        Json::object([
            ("mediatype", media_type(target).unwrap_or_default().into()),
            (
                "uri",
                Json::object([("filename", normalise(source).into()), ("uri", uri.into())]),
            ),
        ])
    });
    Json::object([
        ("input_subdomain", Json::list(inputs)),
        ("output_subdomain", Json::list(outputs)),
    ])
}

/// The option at a JSON pointer of `prov.formats.bco`, else the default.
fn option(options: &Value, pointer: &str, default: Json) -> Json {
    options.pointer(pointer).map_or(default, Json::from)
}

/// The value with the keys of every object sorted, as the etag hashes it.
fn sorted(value: &Json) -> Json {
    match value {
        Json::Object(entries) => {
            let mut entries: Vec<(String, Json)> = (entries.iter())
                .map(|(key, value)| (key.clone(), sorted(value)))
                .collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Json::Object(entries)
        }
        Json::List(items) => Json::List(items.iter().map(sorted).collect()),
        other => other.clone(),
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

/// A random UUID of version 4, from the random source of the system, else from the time and
/// the process id.
fn uuid() -> String {
    let mut bytes = [0_u8; 16];
    let random =
        std::fs::File::open("/dev/urandom").and_then(|mut file| file.read_exact(&mut bytes));
    if random.is_err() {
        let seed = format!("{}-{}", crate::report_format::now(), std::process::id());
        bytes.copy_from_slice(&Sha256::digest(seed.as_bytes())[..16]);
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let text = hex(&bytes);
    format!(
        "{}-{}-{}-{}-{}",
        &text[..8],
        &text[8..12],
        &text[12..16],
        &text[16..20],
        &text[20..]
    )
}
