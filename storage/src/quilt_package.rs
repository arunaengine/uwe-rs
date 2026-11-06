//! The entries of Quilt package manifests, as the Quilt package format describes them.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::Value;

/// An entry of a package: a logical key with the object that holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    pub logical_key: String,
    /// `s3://<bucket>/<key>`, maybe with `?versionId=`.
    pub physical_key: String,
    pub size: u64,
    /// The SHA-256 of the content in hex.
    pub hash: String,
}

/// The entries of a manifest, the lines after its header.
pub(crate) fn entries(text: &str) -> Result<Vec<Entry>, String> {
    let mut entries = Vec::new();
    for line in text.lines().skip(1).filter(|line| !line.trim().is_empty()) {
        let entry: Value = serde_json::from_str(line)
            .map_err(|error| format!("a line of the Quilt manifest is not JSON: {error}"))?;
        let field = |value: &Value| value.as_str().map(str::to_owned);
        let (Some(logical_key), Some(physical_key)) = (
            field(&entry["logical_key"]),
            field(&entry["physical_keys"][0]),
        ) else {
            return Err("an entry of the Quilt manifest has no logical or physical key".into());
        };
        entries.push(Entry {
            logical_key,
            physical_key,
            size: entry["size"].as_u64().unwrap_or(0),
            hash: field(&entry["hash"]["value"]).unwrap_or_default(),
        });
    }
    Ok(entries)
}
