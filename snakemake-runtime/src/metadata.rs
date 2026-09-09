//! The records that Snakemake keeps in `.snakemake/metadata` of the working directory: for each
//! output, the checksums of the inputs that made it. Snakemake's `mtime` trigger does not run
//! a job again for an input that is newer than an output but has the checksum that the record
//! of each output names (Snakemake 9.27, `DAG.is_same_checksum`).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use base64::Engine;
use python_evaluator::Interpreter;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// The folder of the records, in the working directory.
const FOLDER: &str = ".snakemake/metadata";
/// The largest input whose checksum Snakemake compares (`max_checksum_file_size`).
const LARGEST: u64 = 1_000_000;
/// The longest file name of a record; a longer name goes on in folders (`PC_NAME_MAX`).
const NAME_MAX: usize = 255;

pub(crate) fn signature_path(workdir: &Path, output: &str) -> PathBuf {
    workdir.join(record_path(output).replacen(FOLDER, ".snakemake/iunctus-signatures", 1))
}

pub(crate) fn incomplete_path(workdir: &Path, output: &str) -> PathBuf {
    workdir.join(record_path(output).replacen(FOLDER, ".snakemake/iunctus-incomplete", 1))
}

pub(crate) fn mark_incomplete(workdir: &Path, output: &str) -> std::io::Result<()> {
    let path = incomplete_path(workdir, output);
    let parent = path.parent().unwrap_or(workdir);
    std::fs::create_dir_all(parent)?;
    std::fs::File::create(&path)?.sync_all()?;
    let mut directory = Some(parent);
    while let Some(path) = directory {
        std::fs::File::open(path)?.sync_all()?;
        if path == workdir {
            break;
        }
        directory = path.parent();
    }
    Ok(())
}

pub(crate) fn clear_incomplete(workdir: &Path, output: &str) -> std::io::Result<()> {
    let path = incomplete_path(workdir, output);
    std::fs::remove_file(&path)?;
    std::fs::File::open(path.parent().unwrap_or(workdir))?.sync_all()
}

pub(crate) fn signature_changed(workdir: &Path, output: &str, current: &serde_json::Value) -> bool {
    let Some(mut recorded) = std::fs::read(signature_path(workdir, output))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
    else {
        return true;
    };
    let mut current = current.clone();
    let (Some(old), Some(new)) = (
        recorded
            .get_mut("params")
            .and_then(serde_json::Value::as_array_mut),
        current
            .get_mut("params")
            .and_then(serde_json::Value::as_array_mut),
    ) else {
        return true;
    };
    for (old, new) in old.iter_mut().zip(new.iter_mut()) {
        if old.is_null() || new.is_null() {
            *old = serde_json::Value::Null;
            *new = serde_json::Value::Null;
        }
    }
    recorded != current
}

pub(crate) fn protect(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.is_dir() {
        for entry in std::fs::read_dir(path)? {
            protect(&entry?.path())?;
        }
    }
    let mut permissions = metadata.permissions();
    permissions.set_mode(permissions.mode() & !0o222);
    std::fs::set_permissions(path, permissions)
}

pub(crate) fn publish_signature(
    workdir: &Path,
    output: &str,
    signature: &serde_json::Value,
) -> std::io::Result<()> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = signature_path(workdir, output);
    let parent = path.parent().unwrap_or(workdir);
    std::fs::create_dir_all(parent)?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = parent.join(format!(
        ".signature-{}-{nanos}-{}.part",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = std::fs::File::options()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    file.write_all(&serde_json::to_vec(signature).map_err(std::io::Error::other)?)?;
    file.sync_all()?;
    std::fs::rename(&temporary, &path)?;
    std::fs::File::open(parent)?.sync_all()
}

/// Whether every output of a job has a record that names the same checksum for an input, and
/// the input has that checksum now.
pub fn same_checksum(interpreter: &Interpreter, outputs: &[String], input: &str) -> bool {
    let mut recorded: Option<String> = None;
    for output in outputs {
        let Some(checksum) = checksum_of(interpreter, output, input) else {
            return false;
        };
        match &recorded {
            Some(earlier) if *earlier != checksum => return false,
            _ => recorded = Some(checksum),
        }
    }
    let Some(recorded) = recorded else {
        return false;
    };
    // Snakemake takes a checksum without an algorithm as SHA-256.
    let expected = match recorded.split_once(':') {
        Some(("sha256", hex)) => hex,
        Some(_) => return false,
        None => recorded.as_str(),
    };
    let path = interpreter.host.resolve(input);
    let small = interpreter
        .host
        .stat(&path)
        .is_some_and(|stat| stat.size <= LARGEST);
    let Some(content) = small.then(|| interpreter.host.read(&path).ok()).flatten() else {
        return false;
    };
    let digest: String = Sha256::digest(&content)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    digest == expected
}

/// The checksum of an input in the record of an output, if the record has one.
fn checksum_of(interpreter: &Interpreter, output: &str, input: &str) -> Option<String> {
    let path = interpreter.host.resolve(&record_path(output));
    let text = interpreter.host.read(&path).ok()?;
    let record: serde_json::Value = serde_json::from_slice(&text).ok()?;
    let checksum = record.get("input_checksums")?.get(input)?.as_str()?;
    Some(checksum.to_owned())
}

/// Where Snakemake keeps the record of an output: its name in URL-safe Base64, cut into
/// folders whose names start with `@` when it is too long for one name.
fn record_path(output: &str) -> String {
    let name = base64::engine::general_purpose::URL_SAFE.encode(output);
    let parts: Vec<&str> = name
        .as_bytes()
        .chunks(NAME_MAX - 1)
        .map(|chunk| std::str::from_utf8(chunk).unwrap_or_default())
        .collect();
    let last = parts.len().saturating_sub(1);
    let mut path = String::from(FOLDER);
    for (index, part) in parts.iter().enumerate() {
        path.push('/');
        if index < last {
            path.push('@');
        }
        path.push_str(part);
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_are_named_as_snakemake_names_them() {
        assert_eq!(
            record_path("aggregated.txt"),
            ".snakemake/metadata/YWdncmVnYXRlZC50eHQ="
        );
        let long = "x".repeat(300);
        let path = record_path(&long);
        let parts: Vec<&str> = path.split('/').skip(2).collect();
        assert_eq!(parts.len(), 2);
        assert!(parts[0].starts_with('@') && parts[0].len() == NAME_MAX);
    }
}
