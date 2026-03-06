//! The result manifest that the wrapper writes: each collected file with its size.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

/// A collected file: its path relative to the work directory, and its size in bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Collected {
    pub path: String,
    pub size: u64,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("line {line} of the result manifest is not `<size> <path>`: {text}")]
pub struct ManifestError {
    pub line: usize,
    pub text: String,
}

/// Reads the manifest. A file that several patterns matched is listed once, in the order of
/// its first match.
pub fn parse(text: &str) -> Result<Vec<Collected>, ManifestError> {
    let mut files: Vec<Collected> = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let parsed = line
            .split_once(' ')
            .and_then(|(size, path)| Some((size.trim().parse().ok()?, path)));
        let Some((size, path)) = parsed.filter(|(_, path)| !path.is_empty()) else {
            return Err(ManifestError {
                line: index + 1,
                text: line.to_owned(),
            });
        };
        if !files.iter().any(|file| file.path == path) {
            files.push(Collected {
                path: path.to_owned(),
                size,
            });
        }
    }
    Ok(files)
}
