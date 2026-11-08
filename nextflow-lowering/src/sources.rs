//! The sources of a lowered pipeline: its main script, the scripts that define its processes
//! and closures, and its config files, each with a stable id.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use serde_json::{Value as Json, json};

/// The source files of a document by id, in the order they were first named.
#[derive(Clone, Debug)]
pub struct Sources {
    base: PathBuf,
    files: Vec<(String, String)>,
}

impl Sources {
    /// Sources whose locations are relative to `base` where they lie below it, starting with
    /// the main script.
    pub fn new(main: &str, base: &Path) -> Sources {
        let mut sources = Sources {
            base: base.to_owned(),
            files: Vec::new(),
        };
        sources
            .files
            .push(("main".into(), sources.location(Path::new(main))));
        sources
    }

    /// The id of the main script.
    pub fn main(&self) -> String {
        self.files[0].0.clone()
    }

    /// The id of a script or config file, named by its location.
    pub fn script(&mut self, path: &str) -> String {
        let location = self.location(Path::new(path));
        if let Some((id, _)) = self.files.iter().find(|(_, known)| *known == location) {
            return id.clone();
        }
        let mut id: String = (location.trim_start_matches(['/', '.']).chars())
            .map(|letter| match letter {
                'a'..='z' | 'A'..='Z' | '0'..='9' | '_' | '-' | '.' => letter,
                _ => '.',
            })
            .collect();
        while self.files.iter().any(|(known, _)| *known == id) {
            id.push('_');
        }
        self.files.push((id.clone(), location));
        id
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
            .map(|(id, location)| json!({"id": id, "language": "nextflow", "location": location}))
            .collect()
    }
}
