//! Runs a task in a container with the local Docker or Podman, in a scratch directory.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use task_model::TaskSpec;

use crate::job::{CONTROL, ExecutorError, Finished, MOUNTS, Staging, WORK};
use crate::manifest::parse;
use crate::wrapper::{EXIT, MANIFEST, STDIN, wrapper};

/// The local container executor.
pub struct LocalContainer {
    /// The container engine command, `docker` or `podman`.
    pub engine: String,
    /// Where each task gets its scratch directory.
    pub scratch: PathBuf,
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
        std::fs::create_dir_all(&self.scratch)?;
        let root = self.scratch.join(name);
        std::fs::create_dir(&root)?;
        let work = root.join(WORK);
        let control = root.join(CONTROL);
        std::fs::create_dir_all(&work)?;
        std::fs::create_dir_all(&control)?;
        let place = |path: &str| match path.strip_prefix('/') {
            Some(inside) => root.join(MOUNTS).join(inside),
            None => work.join(path),
        };
        for directory in &staging.directories {
            std::fs::create_dir_all(place(directory))?;
        }
        for input in &staging.inputs {
            let target = place(&input.path);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&input.source, &target)?;
            if input.executable {
                std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))?;
            }
        }
        if let Some(stdin) = &staging.stdin {
            std::fs::copy(stdin, control.join(STDIN))?;
        }
        let mut spec = spec.clone();
        if spec.arguments.is_empty() {
            spec.arguments = self.default_command(&image.reference)?;
        }
        let inside = spec.workdir.clone().unwrap_or_else(|| text(&work));
        let script = control.join("wrapper.sh");
        std::fs::write(&script, wrapper(&spec, &inside, &text(&control)))?;
        let owner = std::fs::metadata(&root)?;
        let mut command = Command::new(&self.engine);
        command
            .args(["run", "--rm", "--entrypoint", "/bin/bash"])
            .args(["--user", &format!("{}:{}", owner.uid(), owner.gid())])
            .args(["--volume", &format!("{0}:{0}", text(&root))]);
        if spec.workdir.is_some() {
            command.args(["--volume", &format!("{}:{inside}", text(&work))]);
        }
        for input in spec
            .inputs
            .iter()
            .filter(|input| input.path.starts_with('/'))
        {
            let path = input.path.trim_end_matches('/');
            command.args(["--volume", &format!("{}:{path}", text(&place(path)))]);
        }
        command.args(["--workdir", &inside]);
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

    /// The default command of an image: its entrypoint, then its command.
    fn default_command(&self, reference: &str) -> Result<Vec<String>, ExecutorError> {
        let format = "{{json .Config.Entrypoint}}\n{{json .Config.Cmd}}";
        let engine_error = |message: String| ExecutorError::Engine {
            engine: self.engine.clone(),
            message,
        };
        let output = Command::new(&self.engine)
            .args(["image", "inspect", "--format", format, reference])
            .output()
            .map_err(|error| engine_error(error.to_string()))?;
        if !output.status.success() {
            return Err(engine_error(
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ));
        }
        let mut command = Vec::new();
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let part: Option<Vec<String>> =
                serde_json::from_str(line).map_err(|error| engine_error(error.to_string()))?;
            command.extend(part.unwrap_or_default());
        }
        if command.is_empty() {
            return Err(ExecutorError::NoCommand(reference.to_owned()));
        }
        Ok(command)
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
