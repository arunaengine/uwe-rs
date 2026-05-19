//! The hashed task specification: everything a task computes from, and its cache key.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::Digest;

/// Version of the specification format; any change to the encoding must raise it.
pub const FORMAT: u32 = 2;

/// What a task computes. Only this part of a task goes into the cache key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskSpec {
    pub format: u32,
    /// A cache salt; changing it disowns all earlier results.
    pub namespace: String,
    pub image: Option<Image>,
    /// The program and its arguments, run in the work directory. An empty list runs the
    /// image's default command.
    pub arguments: Vec<String>,
    /// The work directory inside the container, as an absolute path; `None` lets the executor
    /// choose one.
    pub workdir: Option<String>,
    pub environment: BTreeMap<String, EnvironmentValue>,
    pub inputs: Vec<InputFile>,
    pub outputs: Vec<OutputRule>,
    /// The digest of the text that the task reads on standard input.
    pub stdin: Option<Digest>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Image {
    /// For example `quay.io/biocontainers/fastqc@sha256:...` once pinned.
    pub reference: String,
    pub platform: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentValue {
    Literal(String),
    /// The path of the work directory, which differs per executor and so is not a literal.
    WorkDirectory,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputFile {
    /// A path relative to the work directory, or an absolute path inside the container.
    pub path: String,
    pub kind: FileKind,
    /// The file digest, or for a directory the digest of its tree manifest.
    pub digest: Digest,
    pub executable: bool,
    pub writable: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    File,
    Directory,
}

/// A shell glob, relative to the work directory or absolute inside the container. A pattern
/// that ends with `/` names a directory. Drivers apply their exact rules afterwards.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputRule {
    pub pattern: String,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SpecError {
    #[error("unsupported specification format {0}; this build writes format {FORMAT}")]
    Format(u32),
    #[error("the task has neither a program nor an image to run")]
    NoProgram,
    #[error("`{0}` must be a path inside the work directory or an absolute path, without `..`")]
    Path(String),
    #[error("the work directory `{0}` must be an absolute path without `..`")]
    Workdir(String),
    #[error("the input path `{0}` appears more than once")]
    DuplicateInput(String),
    #[error("cannot encode the specification: {0}")]
    Encoding(String),
}

impl TaskSpec {
    pub fn validate(&self) -> Result<(), SpecError> {
        if self.format != FORMAT {
            return Err(SpecError::Format(self.format));
        }
        if self.arguments.is_empty() && self.image.is_none() {
            return Err(SpecError::NoProgram);
        }
        if let Some(workdir) = &self.workdir
            && (!workdir.starts_with('/') || check_path(workdir).is_err())
        {
            return Err(SpecError::Workdir(workdir.clone()));
        }
        let mut seen = BTreeSet::new();
        for input in &self.inputs {
            check_path(&input.path)?;
            if !seen.insert(input.path.as_str()) {
                return Err(SpecError::DuplicateInput(input.path.clone()));
            }
        }
        for output in &self.outputs {
            check_path(&output.pattern)?;
        }
        Ok(())
    }

    /// The RFC 8785 canonical JSON of the specification. Inputs and outputs are sorted, because
    /// their order does not change what the task computes.
    pub fn canonical(&self) -> Result<Vec<u8>, SpecError> {
        self.validate()?;
        let mut sorted = self.clone();
        sorted.inputs.sort_by(|a, b| a.path.cmp(&b.path));
        sorted.outputs.sort();
        sorted.outputs.dedup();
        serde_jcs::to_vec(&sorted).map_err(|error| SpecError::Encoding(error.to_string()))
    }

    pub fn cache_key(&self) -> Result<Digest, SpecError> {
        Ok(Digest::sha256(&self.canonical()?))
    }

    /// Whether the image is pinned to a digest, one condition for sharing results across runs.
    pub fn image_pinned(&self) -> bool {
        self.image
            .as_ref()
            .is_none_or(|image| image.reference.contains("@sha256:"))
    }
}

/// A relative path must stay inside the work directory, whatever the executor mounts it as.
/// An absolute path names a place inside the container other than its root.
fn check_path(path: &str) -> Result<(), SpecError> {
    let escapes = path.split('/').any(|part| part == "..");
    if path.trim_matches('/').is_empty() || escapes || path.contains('\0') {
        return Err(SpecError::Path(path.to_owned()));
    }
    Ok(())
}
