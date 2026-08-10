//! The result manifest that the wrapper writes: each collected file, and each collected
//! directory as one entry.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

/// A collected output: its path as the task named it (relative to the work directory, or
/// absolute), and whether it is a directory. The runner learns the size of a file when it
/// stores it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Collected {
    pub path: String,
    pub directory: bool,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("line {line} of the result manifest is not `file <path>` or `directory <path>`: {text}")]
pub struct ManifestError {
    pub line: usize,
    pub text: String,
}

/// Reads the manifest. A path that several patterns matched is listed once, in the order of
/// its first match.
pub fn parse(text: &str) -> Result<Vec<Collected>, ManifestError> {
    let mut files: Vec<Collected> = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let parsed = match line.split_once(' ') {
            Some(("file", path)) => Some((false, path)),
            Some(("directory", path)) => Some((true, path)),
            _ => None,
        };
        let Some((directory, path)) = parsed.filter(|(_, path)| !path.is_empty()) else {
            return Err(ManifestError {
                line: index + 1,
                text: line.to_owned(),
            });
        };
        if !files.iter().any(|file| file.path == path) {
            files.push(Collected {
                path: path.to_owned(),
                directory,
            });
        }
    }
    Ok(files)
}
