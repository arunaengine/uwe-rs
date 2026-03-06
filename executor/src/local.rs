//! Runs a task in a container with the local Docker or Podman, in a scratch directory.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use task_model::TaskSpec;

use crate::manifest::{Collected, ManifestError, parse};
use crate::wrapper::{EXIT, MANIFEST, wrapper};

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

/// What a task left behind.
#[derive(Debug)]
pub struct Finished {
    /// The tool's exit code; `None` when the wrapper was killed before it recorded one.
    pub exit_code: Option<i32>,
    /// The collected files, relative to the work directory.
    pub files: Vec<Collected>,
    /// The work directory.
    pub work: PathBuf,
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
    /// Runs a task in `<scratch>/<name>`: stages its inputs, runs the wrapper in the task's
    /// image as the current user, and reads the control files. The directory stays for the
    /// caller to take the outputs from.
    pub fn run(
        &self,
        name: &str,
        spec: &TaskSpec,
        staged: &[Staged],
    ) -> Result<Finished, ExecutorError> {
        let image = spec.image.as_ref().ok_or(ExecutorError::NoImage)?;
        let root = self.scratch.join(name);
        if root.exists() {
            std::fs::remove_dir_all(&root)?;
        }
        let work = root.join("work");
        let control = root.join("control");
        std::fs::create_dir_all(&work)?;
        std::fs::create_dir_all(&control)?;
        for file in staged {
            let target = work.join(&file.path);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&file.source, &target)?;
            if file.executable {
                std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))?;
            }
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
            work,
        })
    }
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}
