//! The launch that a compiled job plans with, as the run that the document was compiled for,
//! and the source that pins the config values of its command line.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};
use snakemake_runtime::JobKey;
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
    let location = |id: &str| {
        let source = document.sources.iter().find(|source| source.id == id);
        let source = source.ok_or_else(|| format!("no source {id}"))?;
        Ok::<_, String>(source.location.clone())
    };
    let reference = &rule.task;
    let configs = (reference.configs.iter())
        .map(|id| location(id).map(|location| base.join(location)))
        .collect::<Result<_, _>>()?;
    let params = match &rule.config_values {
        Some(id) => read_values(&location(id)?, base)?,
        None => Vec::new(),
    };
    let targets = match rule.targets.is_empty() {
        true => vec![reference.entry.clone()],
        false => rule.targets.clone(),
    };
    Ok(Launch {
        target: base.join(location(&reference.source)?),
        params,
        configs,
        profiles: reference.profiles.clone(),
        targets,
        until: rule.until.clone(),
        omit_from: rule.omit_from.clone(),
        forcerun: rule.forcerun.clone(),
        forceall: rule.forceall,
        ..Launch::default()
    })
}

/// The identity of the job of `rule` in the plan of its launch.
pub fn job_key(rule: &RuleTask) -> JobKey {
    JobKey {
        rule: rule.task.call.clone(),
        wildcards: rule.wildcards.clone(),
        outputs: Vec::new(),
    }
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
