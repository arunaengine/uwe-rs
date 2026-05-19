//! Runs a task in a container with the local Docker or Podman, in a scratch directory.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use task_model::TaskSpec;

use crate::manifest::{Collected, ManifestError, parse};
use crate::wrapper::{EXIT, MANIFEST, STDERR, STDIN, STDOUT, wrapper};

/// The folders of a task's scratch directory: the work directory and the control files of the
/// wrapper.
const WORK: &str = "work";
const CONTROL: &str = "control";

/// The local container executor.
pub struct LocalContainer {
    /// The container engine command, `docker` or `podman`.
    pub engine: String,
    /// Where each task gets its scratch directory.
    pub scratch: PathBuf,
}

/// A file placed in the work directory before the task starts.
#[derive(Clone, Debug)]
pub struct Staged {
    /// The path relative to the work directory.
    pub path: String,
    /// The local file whose content it gets.
    pub source: PathBuf,
    pub executable: bool,
}

/// What an executor places before a task starts.
#[derive(Clone, Debug, Default)]
pub struct Staging {
    pub files: Vec<Staged>,
    /// The local file that the task reads on standard input.
    pub stdin: Option<PathBuf>,
}

/// What a task left behind in its scratch directory.
#[derive(Debug)]
pub struct Finished {
    /// The tool's exit code; `None` when the wrapper was killed before it recorded one.
    pub exit_code: Option<i32>,
    /// The collected files, relative to the work directory.
    pub files: Vec<Collected>,
    /// The scratch directory, which the caller removes once it has the outputs.
    pub root: PathBuf,
}

impl Finished {
    /// Where an output is on the host.
    pub fn local_path(&self, path: &str) -> PathBuf {
        self.root.join(WORK).join(path)
    }

    /// The file that captured standard output.
    pub fn stdout(&self) -> PathBuf {
        self.root.join(CONTROL).join(STDOUT)
    }

    /// The file that captured standard error.
    pub fn stderr(&self) -> PathBuf {
        self.root.join(CONTROL).join(STDERR)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ExecutorError {
    #[error("the task has no container image; every task runs in a container")]
    NoImage,
    #[error("cannot prepare the task directory: {0}")]
    Io(#[from] std::io::Error),
    #[error("{engine} could not run the task: {message}")]
    Engine { engine: String, message: String },
    #[error(transparent)]
    Manifest(#[from] ManifestError),
}

impl LocalContainer {
    /// Runs a task in `<scratch>/<name>`, a name that must not be in use: stages its inputs,
    /// runs the wrapper in the task's image as the current user, and reads the control files.
    /// The directory stays for the caller to take the outputs from.
    pub fn run(
        &self,
        name: &str,
        spec: &TaskSpec,
        staging: &Staging,
    ) -> Result<Finished, ExecutorError> {
        let image = spec.image.as_ref().ok_or(ExecutorError::NoImage)?;
        std::fs::create_dir_all(&self.scratch)?;
        let root = self.scratch.join(name);
        std::fs::create_dir(&root)?;
        let work = root.join(WORK);
        let control = root.join(CONTROL);
        std::fs::create_dir_all(&work)?;
        std::fs::create_dir_all(&control)?;
        for file in &staging.files {
            let target = work.join(&file.path);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&file.source, &target)?;
            if file.executable {
                std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))?;
            }
        }
        if let Some(stdin) = &staging.stdin {
            std::fs::copy(stdin, control.join(STDIN))?;
        }
        let script = control.join("wrapper.sh");
        std::fs::write(&script, wrapper(spec, &text(&work), &text(&control)))?;
        let owner = std::fs::metadata(&root)?;
        let mut command = Command::new(&self.engine);
        command
            .args(["run", "--rm", "--entrypoint", "/bin/bash"])
            .args(["--user", &format!("{}:{}", owner.uid(), owner.gid())])
            .args(["--volume", &format!("{0}:{0}", text(&root))])
            .args(["--workdir", &text(&work)]);
        if let Some(platform) = &image.platform {
            command.args(["--platform", platform]);
        }
        command.arg(&image.reference).arg(text(&script));
        let output = command.output().map_err(|error| ExecutorError::Engine {
            engine: self.engine.clone(),
            message: error.to_string(),
        })?;
        if !output.status.success() {
            return Err(ExecutorError::Engine {
                engine: self.engine.clone(),
                message: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }
        let exit_code = read(&control.join(EXIT)).and_then(|text| text.trim().parse().ok());
        let files = parse(&read(&control.join(MANIFEST)).unwrap_or_default())?;
        Ok(Finished {
            exit_code,
            files,
            root,
        })
    }
}

impl LocalContainer {
    /// The image pinned to its digest, like `debian@sha256:...`, pulling it first when it is
    /// not local. `None` for an image without a registry digest, such as one built locally.
    pub fn pin(&self, reference: &str) -> Result<Option<String>, ExecutorError> {
        if reference.contains("@sha256:") {
            return Ok(Some(reference.to_owned()));
        }
        let inspect = |engine: &str| {
            Command::new(engine)
                .args([
                    "image",
                    "inspect",
                    "--format",
                    "{{json .RepoDigests}}",
                    reference,
                ])
                .output()
        };
        let engine_error = |message: String| ExecutorError::Engine {
            engine: self.engine.clone(),
            message,
        };
        let mut output = inspect(&self.engine).map_err(|error| engine_error(error.to_string()))?;
        if !output.status.success() {
            let pull = Command::new(&self.engine)
                .args(["pull", "--quiet", reference])
                .output()
                .map_err(|error| engine_error(error.to_string()))?;
            if !pull.status.success() {
                return Err(engine_error(
                    String::from_utf8_lossy(&pull.stderr).trim().to_owned(),
                ));
            }
            output = inspect(&self.engine).map_err(|error| engine_error(error.to_string()))?;
        }
        // The digests look like `["debian@sha256:..."]`; the repository of the first is kept.
        let digests = String::from_utf8_lossy(&output.stdout);
        let first = digests
            .split('"')
            .nth(1)
            .filter(|digest| digest.contains("@sha256:"));
        Ok(first.map(|digest| {
            let hash = digest.split_once('@').map_or(digest, |(_, hash)| hash);
            let name = reference
                .rsplit_once(':')
                .filter(|(_, tag)| !tag.contains('/'));
            let name = name.map_or(reference, |(name, _)| name);
            format!("{name}@{hash}")
        }))
    }
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}
