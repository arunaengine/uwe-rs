//! The sources of a compiled pipeline: its main script, the scripts that define its processes
//! and closures, and its config files, each with a stable id and the digest of its bytes.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};

/// The source files of a document by id, in the order they were first named.
#[derive(Clone, Debug)]
pub struct Sources {
    base: PathBuf,
    /// The id, location and digest of each file.
    files: Vec<(String, String, Option<String>)>,
}

impl Sources {
    /// Sources whose locations are relative to `base` where they lie below it, starting with
    /// the main script.
    pub fn new(main: &str, base: &Path) -> Sources {
        let mut sources = Sources {
            base: base.to_owned(),
            files: Vec::new(),
        };
        let location = sources.location(Path::new(main));
        sources
            .files
            .push(("main".into(), location, digest(Path::new(main))));
        sources
    }

    /// The id of the main script.
    pub fn main(&self) -> String {
        self.files[0].0.clone()
    }

    /// The id of a script or config file, named by its location.
    pub fn script(&mut self, path: &str) -> String {
        if let Some(text) = nextflow_runtime::plugin_script(path) {
            return self.named(path.to_owned(), Some(sha256(text.as_bytes())));
        }
        let location = self.location(Path::new(path));
        self.named(location, digest(Path::new(path)))
    }

    /// The id of the file at a location, which it adds with its digest when it is new.
    fn named(&mut self, location: String, digest: Option<String>) -> String {
        if let Some((id, ..)) = self.files.iter().find(|(_, known, _)| *known == location) {
            return id.clone();
        }
        let name = location
            .split_once("://")
            .map_or(location.as_str(), |(_, rest)| rest);
        let mut id: String = (name.trim_start_matches(['/', '.']).chars())
            .map(|letter| match letter {
                'a'..='z' | 'A'..='Z' | '0'..='9' | '_' | '-' | '.' => letter,
                _ => '.',
            })
            .collect();
        while self.files.iter().any(|(known, ..)| *known == id) {
            id.push('_');
        }
        self.files.push((id.clone(), location, digest));
        id
    }

    /// The source `run`, which pins the params and the resolved config of the run that the
    /// document was compiled for by their digests; it holds no value of either.
    pub fn run(&mut self, params: &Json, config: &str) {
        let params = serde_json::to_string(params).unwrap_or_default();
        let record =
            json!({"params": sha256(params.as_bytes()), "config": sha256(config.as_bytes())});
        let text = record.to_string();
        let encoded: String = (text.bytes())
            .map(|byte| match byte {
                b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                    char::from(byte).to_string()
                }
                other => format!("%{other:02X}"),
            })
            .collect();
        let location = format!("data:application/json,{encoded}");
        self.files
            .push(("run".into(), location, Some(sha256(text.as_bytes()))));
    }

    /// Pins a file that the build read, like a samplesheet, by its location and digest.
    pub fn data(&mut self, location: &str, digest: &str) {
        let location = match location.strip_prefix("file://") {
            Some(path) => self.location(Path::new(path)),
            None if location.contains("://") => location.to_owned(),
            None => self.location(Path::new(location)),
        };
        self.named(location, Some(digest.to_owned()));
    }

    /// The id of a config file of the launch.
    pub fn config(&mut self, path: &Path) -> String {
        self.script(&path.to_string_lossy())
    }

    /// The location of a source file: relative to the base below it, else absolute, without
    /// `.` and `..` parts, as includes like `../modules/x` give them.
    fn location(&self, path: &Path) -> String {
        let path = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
        let mut normal = PathBuf::new();
        for part in path.components() {
            match part {
                std::path::Component::ParentDir => {
                    normal.pop();
                }
                std::path::Component::CurDir => {}
                other => normal.push(other),
            }
        }
        match normal.strip_prefix(&self.base) {
            Ok(relative) if relative.as_os_str().is_empty() => ".".into(),
            Ok(relative) => relative.to_string_lossy().into_owned(),
            Err(_) => path.to_string_lossy().into_owned(),
        }
    }

    /// The sources as the document lists them.
    pub fn list(&self) -> Vec<Json> {
        (self.files.iter())
            .map(|(id, location, digest)| {
                let mut source = json!({"id": id, "language": "nextflow", "location": location});
                if let Some(digest) = digest {
                    source["digest"] = json!(digest);
                }
                source
            })
            .collect()
    }
}

/// The SHA-256 digest of a file, which pins the source that a dialect task loads; a folder,
/// like a project folder, has none.
fn digest(path: &Path) -> Option<String> {
    std::fs::read(path).ok().map(|bytes| sha256(&bytes))
}

fn sha256(bytes: &[u8]) -> String {
    let hex: String = Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("sha256:{hex}")
}
