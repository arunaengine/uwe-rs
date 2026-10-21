//! Directory manifests; the digest of a directory is the digest of its canonical manifest.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde::{Deserialize, Serialize};

use crate::{Digest, SpecError};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tree {
    pub entries: Vec<TreeEntry>,
}

/// A file with its content digest, or an empty directory, which a file list cannot express.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum TreeEntry {
    File {
        path: String,
        digest: Digest,
        size: u64,
        executable: bool,
    },
    EmptyDirectory {
        path: String,
    },
    Symlink {
        path: String,
        target: String,
    },
}

impl TreeEntry {
    pub fn path(&self) -> &str {
        match self {
            Self::File { path, .. }
            | Self::EmptyDirectory { path }
            | Self::Symlink { path, .. } => path,
        }
    }
}

impl Tree {
    /// The RFC 8785 canonical JSON of the manifest, with the entries sorted by path.
    pub fn canonical(&self) -> Result<Vec<u8>, SpecError> {
        let mut sorted = self.entries.clone();
        sorted.sort_by(|a, b| a.path().cmp(b.path()));
        for pair in sorted.windows(2) {
            if pair[0].path() == pair[1].path() {
                return Err(SpecError::DuplicateInput(pair[0].path().to_owned()));
            }
        }
        serde_jcs::to_vec(&Tree { entries: sorted })
            .map_err(|error| SpecError::Encoding(error.to_string()))
    }

    pub fn digest(&self) -> Result<Digest, SpecError> {
        Ok(Digest::sha256(&self.canonical()?))
    }

    /// The total size of the files.
    pub fn size(&self) -> u64 {
        self.entries
            .iter()
            .map(|entry| match entry {
                TreeEntry::File { size, .. } => *size,
                TreeEntry::EmptyDirectory { .. } | TreeEntry::Symlink { .. } => 0,
            })
            .sum()
    }
}
