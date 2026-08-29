//! The hashed task specification: everything a task computes from, and its cache key.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, PathBuf};

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
    /// The containers of a task that runs several at once in one place, like the containers of
    /// an Argo container set: they share the work directory, the inputs and the outputs. Empty
    /// for a task of one container, which the fields above describe. Otherwise the first is
    /// the main container: the fields above repeat its image, arguments, work directory and
    /// environment, it reads the standard input, its standard output is the task's, and it
    /// collects the outputs. An empty list is left out of the encoding, so that the keys of
    /// tasks of one container stay the same.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub containers: Vec<ContainerSpec>,
    /// Whether `arguments` replace the entrypoint of the image, as the `command` of a
    /// container of Kubernetes does, instead of running through it, as the arguments of
    /// `docker run` do. False is left out of the encoding, so that the keys of other tasks
    /// stay the same.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub replaces_entrypoint: bool,
    /// Whether the containers run as Kubernetes runs them: as the user of the image, or the
    /// user and group that `RunAs` names, in the working directory of the image, or in
    /// `workdir`, which is then only where the command starts and not where the work
    /// directory is. Left out of the encoding when `None`, the container then runs as the
    /// user who runs iunctus, in the work directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_as: Option<RunAs>,
}

/// The user and group that a container runs as, like `runAsUser` and `runAsGroup` of a
/// Kubernetes `securityContext`; `None` keeps what the image says.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunAs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<u32>,
}

/// One container of a task that runs several.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContainerSpec {
    /// Unique in the task, and a safe file name.
    pub name: String,
    pub image: Option<Image>,
    /// The program and its arguments; an empty list runs the image's default command.
    pub arguments: Vec<String>,
    /// Where the container starts, as an absolute path; `None` for the work directory, or
    /// for the working directory of the image when the task runs as its image says.
    pub workdir: Option<String>,
    pub environment: BTreeMap<String, EnvironmentValue>,
    /// The containers of the task, by name, that must end with exit code 0 before this one
    /// starts. When one of them fails, this one does not run and fails too.
    pub after: Vec<String>,
    /// How often the container runs its command at most while it fails, at least once.
    pub attempts: u32,
    /// Whether `arguments` replace the entrypoint of the image, as for the task. False is
    /// left out of the encoding.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub replaces_entrypoint: bool,
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
    #[error("the containers of the task are not valid: {0}")]
    Containers(String),
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
        self.check_containers()
    }

    /// The containers of a task that runs several have unique names that are safe as file
    /// names, and wait only for containers of the task, without a cycle.
    fn check_containers(&self) -> Result<(), SpecError> {
        let invalid = |message: String| Err(SpecError::Containers(message));
        let mut names = BTreeSet::new();
        for container in &self.containers {
            let name = container.name.as_str();
            let safe = (name.chars()).all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
            if name.is_empty() || name.starts_with('.') || !safe {
                return invalid(format!("`{name}` is not a safe name"));
            }
            if !names.insert(name) {
                return invalid(format!("`{name}` appears more than once"));
            }
            if container.arguments.is_empty() && container.image.is_none() {
                return Err(SpecError::NoProgram);
            }
        }
        for container in &self.containers {
            if let Some(unknown) =
                (container.after.iter()).find(|name| !names.contains(name.as_str()))
            {
                return invalid(format!(
                    "`{}` waits for `{unknown}`, which it does not have",
                    container.name
                ));
            }
        }
        if self.container_order().is_none() {
            return invalid("the containers wait for each other in a cycle".to_owned());
        }
        Ok(())
    }

    /// The positions of the containers in an order where each comes after those it waits for,
    /// otherwise in the order of the list; `None` when they wait for each other in a cycle.
    pub fn container_order(&self) -> Option<Vec<usize>> {
        let containers = &self.containers;
        let mut order: Vec<usize> = Vec::new();
        while order.len() < containers.len() {
            let next = (0..containers.len()).find(|&position| {
                !order.contains(&position)
                    && (containers[position].after.iter())
                        .all(|name| (order.iter()).any(|&done| containers[done].name == *name))
            })?;
            order.push(next);
        }
        Some(order)
    }

    /// The RFC 8785 canonical JSON of the specification, preserving the sorted format encoding.
    /// The cache key additionally records input order when destinations overlap.
    pub fn canonical(&self) -> Result<Vec<u8>, SpecError> {
        self.validate()?;
        let mut sorted = self.clone();
        sorted.inputs.sort_by(|a, b| a.path.cmp(&b.path));
        sorted.outputs.sort();
        sorted.outputs.dedup();
        serde_jcs::to_vec(&sorted).map_err(|error| SpecError::Encoding(error.to_string()))
    }

    pub fn cache_key(&self) -> Result<Digest, SpecError> {
        let mut bytes = self.canonical()?;
        let paths: Vec<PathBuf> = self
            .inputs
            .iter()
            .map(|input| {
                PathBuf::from(&input.path)
                    .components()
                    .filter(|part| *part != Component::CurDir)
                    .collect()
            })
            .collect();
        let destinations: BTreeSet<PathBuf> = paths.iter().cloned().collect();
        let overlays = destinations.len() != paths.len()
            || paths.iter().any(|path| {
                path.ancestors()
                    .skip(1)
                    .any(|parent| destinations.contains(parent))
            });
        if overlays {
            bytes.extend_from_slice(b"\0iunctus:ordered-input-overlays:v1\0");
            bytes.extend(
                serde_jcs::to_vec(&self.inputs)
                    .map_err(|error| SpecError::Encoding(error.to_string()))?,
            );
        }
        Ok(Digest::sha256(&bytes))
    }

    /// Whether the image is pinned to a digest, one condition for sharing results across runs.
    pub fn image_pinned(&self) -> bool {
        let pinned = |image: &Option<Image>| {
            (image.as_ref()).is_none_or(|image| image.reference.contains("@sha256:"))
        };
        pinned(&self.image) && (self.containers.iter()).all(|container| pinned(&container.image))
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
