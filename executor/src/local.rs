//! Runs a task in a container with the local Docker or Podman, in a scratch directory.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use task_model::TaskSpec;

use crate::engine::ContainerEngine;
use crate::image;
use crate::job::{CONTROL, ExecutorError, Finished, Staging, WORK};
use crate::manifest::parse;
use crate::pool::Pool;
use crate::stage::place;
use crate::wrapper::{EXIT, MANIFEST, wrapper};

/// The local container executor.
#[derive(Debug)]
pub struct LocalContainer {
    pub engine: ContainerEngine,
    /// Where each task gets its scratch directory.
    pub scratch: PathBuf,
    /// The CPUs and memory that tasks share.
    pub pool: Pool,
}

impl LocalContainer {
    /// Runs a task in `<scratch>/<name>`, a name that must not be in use: stages its inputs,
    /// runs the wrapper in the task's image as the current user, and reads the control files.
    /// The directory stays for the caller to take the outputs from.
    ///
    /// The work directory is mounted at the task's `workdir` when it has one, and each input
    /// at an absolute path is mounted there. A task without arguments runs the image's default
    /// command.
    pub fn run(
        &self,
        name: &str,
        spec: &TaskSpec,
        staging: &Staging,
    ) -> Result<Finished, ExecutorError> {
        let image = spec.image.as_ref().ok_or(ExecutorError::NoImage)?;
        let root = self.scratch.join(name);
        let mounts = place(&root, staging)?;
        let work = root.join(WORK);
        let control = root.join(CONTROL);
        let mut spec = spec.clone();
        if spec.arguments.is_empty() {
            spec.arguments = self.default_command(&image.reference)?;
        }
        let inside = spec.workdir.clone().unwrap_or_else(|| text(&work));
        let script = control.join("wrapper.sh");
        std::fs::write(&script, wrapper(&spec, &inside, &text(&control)))?;
        let owner = std::fs::metadata(&root)?;
        let mut command = Command::new(&self.engine.program);
        command
            .args(["run", "--rm", "--entrypoint", "/bin/bash"])
            .args(["--user", &format!("{}:{}", owner.uid(), owner.gid())])
            .args(["--volume", &format!("{0}:{0}", text(&root))]);
        if spec.workdir.is_some() {
            command.args(["--volume", &format!("{}:{inside}", text(&work))]);
        }
        for mount in &mounts {
            let mode = if mount.read_only { ":ro" } else { "" };
            let volume = format!("{}:{}{mode}", text(&mount.source), mount.target);
            command.args(["--volume", &volume]);
        }
        command.args(["--workdir", &inside]);
        if let Some(platform) = &image.platform {
            command.args(["--platform", platform]);
        }
        command.arg(&image.reference).arg(text(&script));
        let output = command.output().map_err(|error| ExecutorError::Engine {
            engine: self.engine.program.clone(),
            message: error.to_string(),
        })?;
        if !output.status.success() {
            return Err(ExecutorError::Engine {
                engine: self.engine.program.clone(),
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

    /// The default command of an image: its entrypoint, then its command.
    fn default_command(&self, reference: &str) -> Result<Vec<String>, ExecutorError> {
        block_on(image::default_command(&self.engine, reference))
    }

    /// The image pinned to its digest, like `debian@sha256:...`, pulling it first when it is
    /// not local. `None` for an image without a registry digest, such as one built locally.
    pub fn pin(&self, reference: &str) -> Result<Option<String>, ExecutorError> {
        block_on(image::pin(&self.engine, reference))
    }
}

/// Runs a future to its end on a runtime of its own, until the task runner runs on Tokio.
fn block_on<T>(
    future: impl std::future::Future<Output = Result<T, ExecutorError>>,
) -> Result<T, ExecutorError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(future)
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}
