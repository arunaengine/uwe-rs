//! Publishes Quilt packages: the manifest of a version with its entries and top hash, and the
//! pointers that name it, as the Quilt package format describes them.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;
use std::sync::Arc;

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::StorageError;
use crate::error::Problem;
use crate::objects::{Objects, problem};
use crate::quilt::{self, Address, WRITTEN, hex};

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

/// What a push adds to the files that the run wrote into a package.
#[derive(Clone, Debug, Default)]
pub struct Push {
    /// A Quilt+ address of the package; the pairs of its query join the metadata.
    pub address: String,
    pub meta: Map<String, Value>,
    pub message: String,
    /// Files that the push writes into the package, by logical key, like a README.
    pub files: Vec<(String, Vec<u8>)>,
    /// The globs of `quilt_summarize.json`; `None` writes none.
    pub summarize: Option<Vec<String>>,
}

/// Writes the manifest of the files of the package and its pointers `<unix seconds>` and
/// `latest`, and returns the top hash.
pub fn push(push: Push) -> Result<String, StorageError> {
    let target = push.address.clone();
    crate::blocking::run(async move {
        let failed = |problem: Problem| StorageError::Failed {
            action: "publish",
            address: target.clone(),
            provider: "Quilt".to_owned(),
            message: problem.message,
            retryable: problem.retryable,
        };
        publish(push).await.map_err(failed)
    })
}

/// Keeps a file that went to a plain `s3://` address for the push of a package, with the size
/// and SHA-256 of its local copy.
pub fn record(address: &str, physical: &str, file: &Path) -> Result<(), StorageError> {
    let (address, physical, file) = (address.to_owned(), physical.to_owned(), file.to_owned());
    crate::blocking::run(async move {
        let failed = |problem: Problem| StorageError::Failed {
            action: "record",
            address: physical.clone(),
            provider: "Quilt".to_owned(),
            message: problem.message,
            retryable: problem.retryable,
        };
        let parsed = Address::parse(&address).map_err(|message| failed(Problem::fixed(message)))?;
        let size = (tokio::fs::metadata(&file).await)
            .map_err(|error| failed(Problem::local(&file, error)))?
            .len();
        let item = Entry {
            logical_key: parsed.path.clone(),
            physical_key: physical.clone(),
            size,
            hash: quilt::file_hash(&file).await.map_err(failed)?,
        };
        let objects = Arc::new(Objects::new(quilt::environment()).map_err(failed)?);
        quilt::remember(&parsed.bucket, &parsed.package, item, &objects);
        Ok(())
    })
}

async fn publish(push: Push) -> Result<String, Problem> {
    let address = Address::parse(&push.address).map_err(Problem::fixed)?;
    let (mut items, objects) = taken(&address)?;
    let operator = objects.at(&format!("s3://{}/", address.bucket))?;
    if quilt::read(&operator, ".quilt/workflows/config.yml")
        .await?
        .is_some()
    {
        tracing::warn!(
            "uwe does not check the Quilt workflows of {}",
            address.bucket
        );
    }
    let mut files = push.files;
    if let Some(globs) = &push.summarize {
        let mut keys: Vec<&str> = items.iter().map(|item| item.logical_key.as_str()).collect();
        keys.extend(files.iter().map(|(key, _)| key.as_str()));
        let summary = summary(globs, &keys);
        files.push(("quilt_summarize.json".to_owned(), summary.into_bytes()));
    }
    for (key, bytes) in files {
        let object = format!("{}/{key}", address.package);
        items.retain(|item| item.logical_key != key);
        items.push(Entry {
            physical_key: format!("s3://{}/{object}", address.bucket),
            size: bytes.len() as u64,
            hash: hex(&Sha256::digest(&bytes)),
            logical_key: key,
        });
        operator.write(&object, bytes).await.map_err(problem)?;
    }
    items.sort_by(|left, right| {
        let parts = |item: &Entry| {
            item.logical_key
                .split('/')
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        parts(left).cmp(&parts(right))
    });
    let mut meta = push.meta;
    for (key, value) in &address.meta {
        meta.insert(key.clone(), Value::String(value.clone()));
    }
    if let Some(workflow) = &address.workflow {
        tracing::warn!("uwe does not check the Quilt workflow {workflow}; the package names none");
    }
    let header = json!({ "version": "v0", "user_meta": meta, "message": push.message });
    let (manifest, hash) = manifest(&header, &items);
    let package = &address.package;
    operator
        .write(&format!(".quilt/packages/{hash}"), manifest)
        .await
        .map_err(problem)?;
    let seconds = jiff::Timestamp::now().as_second();
    for name in [seconds.to_string(), "latest".to_owned()] {
        let pointer = format!(".quilt/named_packages/{package}/{name}");
        operator
            .write(&pointer, hash.clone())
            .await
            .map_err(problem)?;
    }
    Ok(hash)
}

/// The files written into the package, and the client that wrote them.
fn taken(address: &Address) -> Result<(Vec<Entry>, Arc<Objects>), Problem> {
    let mut written = WRITTEN.lock().unwrap_or_else(|poison| poison.into_inner());
    let (mine, others): (Vec<_>, Vec<_>) = std::mem::take(&mut *written)
        .into_iter()
        .partition(|kept| kept.bucket == address.bucket && kept.package == address.package);
    *written = others;
    let objects = match mine.first() {
        Some(kept) => kept.objects.clone(),
        None => Arc::new(Objects::new(quilt::environment())?),
    };
    Ok((mine.into_iter().map(|kept| kept.item).collect(), objects))
}

/// The manifest, JSON Lines of the header and the entries, and its top hash: the SHA-256 of the
/// canonical JSON of the header, then of each entry without its physical keys.
fn manifest(header: &Value, items: &[Entry]) -> (String, String) {
    let mut digest = Sha256::new();
    let mut top = header.clone();
    if let Some(top) = top.as_object_mut() {
        top.retain(|key, _| ["message", "user_meta", "version"].contains(&key.as_str()));
    }
    digest.update(canonical(&top));
    let mut text = format!("{}\n", header);
    for item in items {
        let hash = json!({ "type": "SHA256", "value": item.hash });
        let entry = json!({
            "logical_key": item.logical_key, "physical_keys": [item.physical_key],
            "size": item.size, "hash": hash, "meta": {},
        });
        text.push_str(&format!("{entry}\n"));
        let hashed = json!({
            "hash": hash, "logical_key": item.logical_key, "meta": {}, "size": item.size,
        });
        digest.update(canonical(&hashed));
    }
    (text, hex(&digest.finalize()))
}

/// Compact JSON with sorted keys and every character beyond ASCII escaped, as Python's
/// `json.dumps(sort_keys=True, separators=(',', ':'))` writes it.
fn canonical(value: &Value) -> String {
    let mut text = String::new();
    for character in value.to_string().chars() {
        if character.is_ascii() {
            text.push(character);
            continue;
        }
        let mut units = [0; 2];
        for unit in character.encode_utf16(&mut units) {
            text.push_str(&format!("\\u{unit:04x}"));
        }
    }
    text
}

/// `quilt_summarize.json`: for each glob in order, the keys it matches, as Jackson prints them.
fn summary(globs: &[String], keys: &[&str]) -> String {
    let mut found: Vec<&str> = Vec::new();
    for glob in globs {
        for key in keys {
            if matches(glob.trim().as_bytes(), key.as_bytes()) && !found.contains(key) {
                found.push(key);
            }
        }
    }
    let items: Vec<String> = (found.iter())
        .map(|key| {
            let name = key.rsplit('/').next().unwrap_or(key);
            format!(
                "{{\n  \"path\" : {},\n  \"title\" : {}\n}}",
                Value::from(*key),
                Value::from(name)
            )
        })
        .collect();
    match items.is_empty() {
        true => "[ ]".to_owned(),
        false => format!("[ {} ]", items.join(", ")),
    }
}

/// A glob where `*` and `?` stay inside one folder and `**/` spans any number of folders.
fn matches(pattern: &[u8], text: &[u8]) -> bool {
    match pattern {
        [] => text.is_empty(),
        [b'*', b'*', b'/', rest @ ..] => {
            matches(rest, text)
                || (0..text.len()).any(|at| text[at] == b'/' && matches(rest, &text[at + 1..]))
        }
        [b'*', rest @ ..] => (0..=text.len())
            .take_while(|&at| at == 0 || text[at - 1] != b'/')
            .any(|at| matches(rest, &text[at..])),
        [b'?', rest @ ..] => text.first().is_some_and(|&c| c != b'/') && matches(rest, &text[1..]),
        [c, rest @ ..] => text.first() == Some(c) && matches(rest, &text[1..]),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(key: &str) -> Entry {
        Entry {
            logical_key: key.into(),
            physical_key: format!("s3://b/p/q/{key}"),
            size: 1,
            hash: "00".into(),
        }
    }

    #[test]
    fn hashes_canonical_json() {
        let header = json!({ "version": "v0", "message": "m", "user_meta": { "b": 1, "a": "é" } });
        let (text, hash) = manifest(&header, &[entry("a.txt")]);
        let mut expected = Sha256::new();
        expected.update(r#"{"message":"m","user_meta":{"a":"\u00e9","b":1},"version":"v0"}"#);
        expected.update(
            r#"{"hash":{"type":"SHA256","value":"00"},"logical_key":"a.txt","meta":{},"size":1}"#,
        );
        assert_eq!(hash, hex(&expected.finalize()));
        assert_eq!(text.lines().count(), 2);
        assert_eq!(entries(&text).expect("entries"), [entry("a.txt")]);
    }

    #[test]
    fn summarizes_like_fixture() {
        let globs: Vec<String> = "*.md,*.html,*.?sv,*.pdf,igv.json,**/multiqc_report.html"
            .split(',')
            .map(str::to_owned)
            .collect();
        let keys = [
            "README_NF_QUILT.md",
            "out/report.html",
            "out/table.tsv",
            "a/b/multiqc_report.html",
        ];
        let expected = "[ {\n  \"path\" : \"README_NF_QUILT.md\",\n  \"title\" : \"README_NF_QUILT.md\"\n}, \
                        {\n  \"path\" : \"a/b/multiqc_report.html\",\n  \"title\" : \"multiqc_report.html\"\n} ]";
        assert_eq!(summary(&globs, &keys), expected);
        assert!(matches(b"**/x.html", b"x.html"));
        assert!(!matches(b"*.tsv", b"out/table.tsv"));
        assert_eq!(summary(&globs, &[]), "[ ]");
    }
}
