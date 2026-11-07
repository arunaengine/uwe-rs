//! nf-snapshot 0.3.0: compares the files a run published with a JSON snapshot in the pipeline,
//! and fails the run when they differ; writes the snapshot when it is new or asked for.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};

use md5::{Digest, Md5};
use nextflow_config::Config;
use nextflow_evaluator::interpreter::glob::matches;
use nextflow_evaluator::value::Value;

use crate::catalogue::{Kind, Plugin, State};
use crate::observer::{Event, Observer, Start};

/// The `update` command, which rewrites a snapshot without a run, is not implemented.
pub(crate) const PLUGIN: Plugin = Plugin {
    scopes: &["snapshot"],
    observer: Some(activate),
    ..Plugin::new("nf-snapshot", Kind::Observer, State::Partial)
};

fn activate(config: &Config, _: &std::path::Path) -> Result<Option<Box<dyn Observer>>, String> {
    let get = |key: &str| {
        config
            .get(&["snapshot", key])
            .filter(|value| !matches!(value, Value::Null))
    };
    if !get("enabled").is_some_and(|value| value.truth()) {
        return Ok(None);
    }
    let Some(file) = get("file")
        .map(|value| value.to_string())
        .filter(|file| !file.is_empty())
    else {
        return Err(
            "nf-snapshot: `snapshot.file` must be set when `snapshot.enabled = true`".to_owned(),
        );
    };
    let list = |key: &str| match get(key) {
        Some(Value::List(list)) => list.borrow().items.iter().map(Value::to_string).collect(),
        Some(other) => vec![other.to_string()],
        None => Vec::new(),
    };
    let update = get("update").is_some_and(|value| value.truth())
        || std::env::var("NXF_SNAPSHOT_UPDATE").is_ok_and(|value| value == "true");
    Ok(Some(Box::new(Snapshot {
        file,
        base: get("baseDir").map(|value| value.to_string()),
        update,
        fail_unexpected: get("failOnUnexpected").is_some_and(|value| value.truth()),
        ignore: list("ignore"),
        name_only: list("nameOnly"),
        size_only: list("sizeOnly"),
        path: PathBuf::new(),
        root: PathBuf::new(),
        published: Vec::new(),
        checked: false,
    })))
}

/// The settings of a check and the files the run published.
struct Snapshot {
    /// The snapshot file as the config gives it, below the project directory when relative.
    file: String,
    base: Option<String>,
    update: bool,
    fail_unexpected: bool,
    ignore: Vec<String>,
    name_only: Vec<String>,
    size_only: Vec<String>,
    /// The snapshot file and the output root, known once the run starts.
    path: PathBuf,
    root: PathBuf,
    published: Vec<PathBuf>,
    checked: bool,
}

impl Observer for Snapshot {
    fn notify(&mut self, event: &Event) {
        match event {
            Event::RunStarted(start) => self.started(start),
            Event::FilePublished { target, .. } => {
                let target = normal(Path::new(target));
                if !self.published.contains(&target) {
                    self.published.push(target);
                }
            }
            Event::RunEnded(end) if !end.success && !self.checked => {
                tracing::warn!("nf-snapshot: run did not succeed, skipping snapshot check");
            }
            _ => {}
        }
    }

    fn check(&mut self) -> Result<(), String> {
        self.checked = true;
        let captured = self.capture()?;
        if captured.is_empty() {
            return Err(format!(
                "nf-snapshot: no published files found under {}. Set `snapshot.baseDir` if \
                 outputs are elsewhere",
                self.root.display()
            ));
        }
        if self.update || !self.path.exists() {
            write(&self.path, &captured)
                .map_err(|error| format!("nf-snapshot: check failed - {error}"))?;
            tracing::info!(
                "nf-snapshot: wrote {} files to {}",
                captured.len(),
                self.path.display()
            );
            return Ok(());
        }
        let text = std::fs::read_to_string(&self.path)
            .map_err(|error| format!("nf-snapshot: check failed - {error}"))?;
        let document: serde_json::Value = serde_json::from_str(&text)
            .map_err(|error| format!("nf-snapshot: check failed - {error}"))?;
        let Some(recorded) = document.get("files").and_then(serde_json::Value::as_object) else {
            return Err(format!(
                "nf-snapshot: {} has no `files` key",
                self.path.display()
            ));
        };
        self.compare(recorded, &captured)
    }
}

impl Snapshot {
    /// Resolves the snapshot file against the project directory, and the output root: the
    /// config's `baseDir`, `outputDir` or `params.outdir`, else `results`.
    fn started(&mut self, start: &Start) {
        let launch = Path::new(&start.launch_dir);
        self.path = normal(&Path::new(&start.project_dir).join(&self.file));
        let configured = |value: &serde_json::Value| {
            value
                .as_str()
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
        };
        let root = (self.base.clone())
            .or_else(|| configured(&start.config["outputDir"]))
            .or_else(|| configured(&start.params["outdir"]))
            .unwrap_or_else(|| "results".to_owned());
        self.root = normal(&launch.join(root));
    }

    /// Each published file below the output root, by its path relative to the root, with its
    /// MD5 sum, its size or nothing, as the patterns ask.
    fn capture(&self) -> Result<BTreeMap<String, Option<String>>, String> {
        let mut captured = BTreeMap::new();
        for target in &self.published {
            if !target.starts_with(&self.root) {
                continue;
            }
            let mut files = Vec::new();
            walk(target, &mut files).map_err(|error| failed(&error))?;
            for file in files {
                let Ok(relative) = file.strip_prefix(&self.root) else {
                    continue;
                };
                let relative = relative.to_string_lossy().replace('\\', "/");
                let matching = |patterns: &[String]| {
                    patterns.iter().any(|pattern| matches(pattern, &relative))
                };
                if matching(&self.ignore) {
                    continue;
                }
                let value = if matching(&self.name_only) {
                    None
                } else if matching(&self.size_only) {
                    let size = std::fs::metadata(&file)
                        .map_err(|error| failed(&error))?
                        .len();
                    Some(format!("size:{size}"))
                } else {
                    Some(digest(&file).map_err(|error| failed(&error))?)
                };
                captured.insert(relative, value);
            }
        }
        Ok(captured)
    }

    /// Compares the capture with the snapshot: missing and changed files fail the run, files
    /// that are not in the snapshot only warn, unless the config says otherwise.
    fn compare(
        &self,
        recorded: &serde_json::Map<String, serde_json::Value>,
        captured: &BTreeMap<String, Option<String>>,
    ) -> Result<(), String> {
        let mut paths: Vec<&String> = recorded.keys().chain(captured.keys()).collect();
        paths.sort();
        paths.dedup();
        let (mut failures, mut unexpected) = (Vec::new(), Vec::new());
        for path in paths {
            let line = |kind: &str| format!("  {:<12}{path}", format!("{kind}:"));
            match (recorded.get(path.as_str()), captured.get(path)) {
                (Some(_), None) => failures.push(line("missing")),
                (None, Some(_)) if self.fail_unexpected => failures.push(line("unexpected")),
                (None, Some(_)) => unexpected.push(line("unexpected")),
                (Some(serde_json::Value::String(old)), Some(Some(new))) if old != new => {
                    failures.push(line("changed"));
                }
                _ => {}
            }
        }
        let file = self.path.display();
        if !unexpected.is_empty() {
            tracing::warn!(
                "nf-snapshot: files not in {file}\n{}\nSet `snapshot.failOnUnexpected = true` to \
                 fail on these",
                unexpected.join("\n")
            );
        }
        if failures.is_empty() {
            tracing::info!("nf-snapshot: {} files match {file}", recorded.len());
            return Ok(());
        }
        tracing::error!(
            "nf-snapshot: {} difference(s) from snapshot",
            failures.len()
        );
        Err(format!(
            "nf-snapshot: outputs differ from {file}\n{}\nRe-run with NXF_SNAPSHOT_UPDATE=true to \
             accept the changes",
            failures.join("\n")
        ))
    }
}

fn failed(error: &std::io::Error) -> String {
    format!("nf-snapshot: check failed - {error}")
}

/// The regular files of a published path: itself, or the files below a folder, following
/// links. A folder that cannot be read is an error, so that no snapshot misses its files.
fn walk(path: &Path, into: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if !path.is_dir() {
        if path.is_file() {
            into.push(path.to_owned());
        }
        return Ok(());
    }
    for entry in std::fs::read_dir(path)? {
        walk(&entry?.path(), into)?;
    }
    Ok(())
}

/// The hex MD5 of a file's content, of the unpacked content for a `.gz` file.
fn digest(path: &Path) -> std::io::Result<String> {
    let file = std::fs::File::open(path)?;
    let mut reader: Box<dyn std::io::Read> = match path.to_string_lossy().ends_with(".gz") {
        true => Box::new(flate2::read::MultiGzDecoder::new(file)),
        false => Box::new(file),
    };
    let mut hasher = Md5::new();
    let mut buffer = [0_u8; 65_536];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// The snapshot file: the paths in text order, two spaces before `files`, four before each.
fn write(path: &Path, files: &BTreeMap<String, Option<String>>) -> std::io::Result<()> {
    let quote = |text: &str| serde_json::Value::from(text).to_string();
    let entries: Vec<String> = (files.iter())
        .map(|(path, value)| {
            let value = value.as_deref().map_or_else(|| "null".to_owned(), quote);
            format!("    {}: {value}", quote(path))
        })
        .collect();
    let text = format!("{{\n  \"files\": {{\n{}\n  }}\n}}\n", entries.join(",\n"));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, text)
}

/// A path without `.` and `..` parts.
fn normal(path: &Path) -> PathBuf {
    let mut normal = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                normal.pop();
            }
            Component::CurDir => {}
            other => normal.push(other),
        }
    }
    normal
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    /// A folder that cannot be read fails the walk instead of leaving its files out.
    #[test]
    fn fails_unreadable_folders() {
        let folder = tempfile::tempdir().expect("a folder");
        let hidden = folder.path().join("hidden");
        std::fs::create_dir(&hidden).expect("makes the folder");
        std::fs::write(hidden.join("a.txt"), "a").expect("writes");
        std::fs::set_permissions(&hidden, std::fs::Permissions::from_mode(0o000)).expect("locks");
        // Root reads any folder; then there is nothing to fail.
        let locked = std::fs::read_dir(&hidden).is_err();
        let mut files = Vec::new();
        let walked = super::walk(folder.path(), &mut files);
        std::fs::set_permissions(&hidden, std::fs::Permissions::from_mode(0o755)).expect("opens");
        assert!(!locked || walked.is_err(), "{files:?}");
    }
}
