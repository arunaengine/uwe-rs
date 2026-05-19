//! The result manifest that the wrapper writes: each collected file with its size, and each
//! collected directory as one entry.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

/// A collected output: its path as the task named it (relative to the work directory, or
/// absolute), whether it is a directory, and the size of a file in bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Collected {
    pub path: String,
    pub directory: bool,
    /// Zero for a directory.
    pub size: u64,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error(
    "line {line} of the result manifest is not `file <size> <path>` or `directory <path>`: {text}"
)]
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
            Some(("file", rest)) => rest
                .split_once(' ')
                .and_then(|(size, path)| Some((false, size.parse().ok()?, path))),
            Some(("directory", path)) => Some((true, 0, path)),
            _ => None,
        };
        let Some((directory, size, path)) = parsed.filter(|(_, _, path)| !path.is_empty()) else {
            return Err(ManifestError {
                line: index + 1,
                text: line.to_owned(),
            });
        };
        if !files.iter().any(|file| file.path == path) {
            files.push(Collected {
                path: path.to_owned(),
                directory,
                size,
            });
        }
    }
    Ok(files)
}
