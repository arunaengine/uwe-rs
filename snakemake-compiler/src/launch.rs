//! The launch that a compiled job plans with, as the run that the document was compiled for,
//! and the source that pins the config values of its command line.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};
use snakemake_runtime::{JobKey, PlannedJob};
use wir::{Document, RuleTask};
use workflow_graph::Launch;

/// The id of the source with the config values of the command line.
pub(crate) const VALUES: &str = "config.values";

const DATA: &str = "data:application/json,";

/// The source of config values `name=value` as a JSON list of pairs in a `data:` location,
/// pinned by the digest of that list.
pub(crate) fn values(params: &[(String, String)]) -> Json {
    let text = json!(params).to_string();
    let encoded: String = (text.bytes())
        .map(|byte| match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                char::from(byte).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect();
    let digest = Sha256::digest(text.as_bytes());
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    json!({"id": VALUES, "language": "snakemake", "location": format!("{DATA}{encoded}"),
        "digest": format!("sha256:{hex}")})
}

/// The launch that the job of `rule` plans with: its Snakefile and config files below `base`,
/// its profiles, targets, config values and options; the caller sets the scratch folder.
pub fn job_launch(document: &Document, rule: &RuleTask, base: &Path) -> Result<Launch, String> {
    let location = |id: &str| located(document, id).map(|location| base.join(location));
    let reference = &rule.task;
    let configs = reference.configs.iter().map(|id| location(id));
    let run = Launch {
        target: location(&reference.source)?,
        configs: configs.collect::<Result<_, _>>()?,
        ..Launch::default()
    };
    Ok(plan_launch(rule, rule_values(document, rule, base)?, run))
}

/// The launch `run` with the profiles, targets, options and config values `params` of the
/// plan of `rule`.
pub fn plan_launch(rule: &RuleTask, params: Vec<(String, String)>, run: Launch) -> Launch {
    let targets = match rule.targets.is_empty() {
        true => vec![rule.task.entry.clone()],
        false => rule.targets.clone(),
    };
    Launch {
        params,
        profiles: rule.task.profiles.clone(),
        targets,
        until: rule.until.clone(),
        omit_from: rule.omit_from.clone(),
        forcerun: rule.forcerun.clone(),
        forceall: rule.forceall,
        ..run
    }
}

/// The config values of the command line that the plan of `rule` has, from the sources of
/// `document`, whose relative locations start from `base`.
pub fn rule_values(
    document: &Document,
    rule: &RuleTask,
    base: &Path,
) -> Result<Vec<(String, String)>, String> {
    match &rule.config_values {
        Some(id) => read_values(&located(document, id)?, base),
        None => Ok(Vec::new()),
    }
}

fn located(document: &Document, id: &str) -> Result<String, String> {
    let source = document.sources.iter().find(|source| source.id == id);
    Ok(source
        .ok_or_else(|| format!("no source {id}"))?
        .location
        .clone())
}

/// The identity of the job of `rule` in the plan of its launch.
pub fn job_key(rule: &RuleTask) -> JobKey {
    JobKey {
        rule: rule.task.call.clone(),
        wildcards: rule.wildcards.clone(),
        outputs: Vec::new(),
    }
}

/// Whether the jobs that a run planned hold the job of `rule`, with the same group and file
/// layout, so that the run runs the plan that the document was compiled from.
pub fn plans_job(jobs: &[PlannedJob], rule: &RuleTask) -> bool {
    let named = |key: &JobKey, job: &PlannedJob| {
        job.key.rule == key.rule && job.key.wildcards == key.wildcards
    };
    let (key, group) = (job_key(rule), group_keys(rule));
    let digest = rule.layout.as_ref();
    // A run groups only jobs that need work: it plans a compiled group whose jobs need no
    // work, or all but one, as single jobs, with the same files.
    let alone =
        |key: &JobKey| (jobs.iter()).position(|job| named(key, job) && job.group.is_empty());
    let members: Option<Vec<usize>> = std::iter::once(&key).chain(&group).map(alone).collect();
    let apart = members
        .filter(|_| !group.is_empty())
        .is_some_and(|members| {
            let needed = members
                .iter()
                .filter(|member| jobs[**member].needed)
                .count();
            needed <= 1 && digest.is_none_or(|digest| *digest == group_layout(jobs, &members))
        });
    apart
        || (0..jobs.len()).any(|index| {
            let job = &jobs[index];
            let layout = digest.is_none_or(|digest| *digest == layout(jobs, index));
            named(&key, job) && job.group == group && layout
        })
}

/// The identities of the other jobs of the group of `rule`.
pub fn group_keys(rule: &RuleTask) -> Vec<JobKey> {
    (rule.group.iter())
        .map(|member| JobKey {
            rule: member.rule.clone(),
            wildcards: member.wildcards.clone(),
            outputs: Vec::new(),
        })
        .collect()
}

/// The digest of the input and output files of a planned job and of its inputs that other
/// jobs make, by position.
pub(crate) fn layout(jobs: &[PlannedJob], index: usize) -> String {
    let job = &jobs[index];
    let outputs: Vec<&str> = job.outputs.iter().map(|file| file.path.as_str()).collect();
    let made = job
        .producers
        .iter()
        .map(|producer| producer.input)
        .collect();
    digest(&job.inputs, &outputs, made)
}

/// The layout of the single jobs `members` as one group: the files they read that none of
/// them makes, each once, the files of each in turn, and those inputs that others make.
fn group_layout(jobs: &[PlannedJob], members: &[usize]) -> String {
    let outputs: Vec<&str> = (members.iter())
        .flat_map(|member| jobs[*member].outputs.iter().map(|file| file.path.as_str()))
        .collect();
    let mut inputs: Vec<String> = Vec::new();
    for file in members.iter().flat_map(|member| &jobs[*member].inputs) {
        if !outputs.contains(&file.as_str()) && !inputs.contains(file) {
            inputs.push(file.clone());
        }
    }
    let made = (members.iter())
        .flat_map(|member| (jobs[*member].producers.iter()).map(move |producer| (member, producer)))
        .filter(|(_, producer)| !members.contains(&producer.job))
        .filter_map(|(member, producer)| {
            let file = &jobs[*member].inputs[producer.input];
            inputs.iter().position(|input| input == file)
        })
        .collect();
    digest(&inputs, &outputs, made)
}

fn digest(inputs: &[String], outputs: &[&str], mut made: Vec<usize>) -> String {
    made.sort_unstable();
    made.dedup();
    let text = json!([inputs, outputs, made]).to_string();
    let digest = Sha256::digest(text.as_bytes());
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("sha256:{hex}")
}

/// Moves the config values that `document` holds in a `data:` source to the location that
/// `place` writes their text to, which keeps the digest; a document without them stays.
pub fn place_values(
    document: &mut Document,
    place: impl FnOnce(&str) -> Result<String, String>,
) -> Result<(), String> {
    let source = document
        .sources
        .iter_mut()
        .find(|source| source.id == VALUES);
    let Some(source) = source else {
        return Ok(());
    };
    let Some(encoded) = source.location.strip_prefix(DATA) else {
        return Ok(());
    };
    let text = decode(encoded).ok_or("a config value source is not percent-encoded")?;
    source.location = place(&text)?;
    Ok(())
}

/// The config values of a source: a `data:` location, or a file below `base`.
fn read_values(location: &str, base: &Path) -> Result<Vec<(String, String)>, String> {
    let text = match location.strip_prefix(DATA) {
        Some(encoded) => decode(encoded).ok_or("a config value source is not percent-encoded")?,
        None => std::fs::read_to_string(base.join(location)).map_err(|error| error.to_string())?,
    };
    serde_json::from_str(&text).map_err(|error| format!("config values: {error}"))
}

fn decode(encoded: &str) -> Option<String> {
    let mut bytes = Vec::new();
    let mut rest = encoded.as_bytes();
    while let Some((first, tail)) = rest.split_first() {
        if *first == b'%' {
            let hex = std::str::from_utf8(tail.get(..2)?).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
            rest = &tail[2..];
        } else {
            bytes.push(*first);
            rest = tail;
        }
    }
    String::from_utf8(bytes).ok()
}
