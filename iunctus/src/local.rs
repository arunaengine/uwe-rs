//! The local container executor behind the engine's executor interface: each task runs in
//! Docker or Podman through the task runner, which reuses results from the cache.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use content_store::Store;
use executor::local::LocalContainer;
use task_model::{
    Digest, EnvironmentValue, FORMAT, FileKind, Image, InputFile, OutputRule, Placement,
    ReuseScope, TaskSpec, Tree,
};
use task_runner::Runner;
use workflow_graph::{ExecuteError, Executor, Outcome, OutputFile, Task};

pub struct Local {
    runner: Runner<LocalContainer>,
    /// The digest of each image, or `None` for an image without one, as found once per run.
    pinned: HashMap<String, Option<String>>,
    /// Prints one line per task when set.
    pub verbose: bool,
}

impl Local {
    /// An executor whose results go to the store and whose tasks run in `scratch`.
    pub fn new(store: &Path, engine: &str, scratch: &Path, run: &str) -> Result<Self, String> {
        let store = Store::local(store).map_err(|error| error.to_string())?;
        let executor = LocalContainer {
            engine: engine.to_owned(),
            scratch: scratch.to_owned(),
        };
        let runner = Runner::new(store, executor, run, &scratch.join("downloads"))
            .map_err(|error| error.to_string())?;
        Ok(Local {
            runner,
            pinned: HashMap::new(),
            verbose: true,
        })
    }

    /// The image pinned to its digest when it has one.
    fn pin(&mut self, image: &str) -> Result<String, String> {
        if !self.pinned.contains_key(image) {
            let digest = self
                .runner
                .executor()
                .pin(image)
                .map_err(|error| error.to_string())?;
            self.pinned.insert(image.to_owned(), digest);
        }
        Ok(self.pinned[image]
            .clone()
            .unwrap_or_else(|| image.to_owned()))
    }
}

impl Executor for Local {
    fn execute(&mut self, task: &Task) -> Result<Outcome, ExecuteError> {
        let problem = |error: &dyn std::fmt::Display| ExecuteError::Executor {
            task: task.name.clone(),
            message: error.to_string(),
        };
        let unsupported = |message: String| ExecuteError::Unsupported {
            task: task.name.clone(),
            message,
        };
        let image = match &task.image {
            Some(image) => Some(self.pin(image).map_err(|error| problem(&error))?),
            None => None,
        };
        let mut inputs = Vec::new();
        for input in &task.inputs {
            let source = Path::new(&input.source);
            let (kind, digest) = match &input.content {
                Some(content) => (FileKind::File, self.runner.import_bytes(content.as_bytes())),
                None if input.source.contains("://") => {
                    return Err(unsupported(format!(
                        "remote input files are not supported yet: {}",
                        input.source
                    )));
                }
                // A directory that the workflow makes itself, like an empty CWL literal.
                None if input.directory && input.source.is_empty() => (
                    FileKind::Directory,
                    self.runner.import_tree(&Tree::default()),
                ),
                None if input.directory || source.is_dir() => (
                    FileKind::Directory,
                    self.runner
                        .import_directory(source)
                        .map(|(digest, _)| digest),
                ),
                None => (
                    FileKind::File,
                    self.runner.import(source).map(|(digest, _)| digest),
                ),
            };
            inputs.push(InputFile {
                path: input.path.clone(),
                kind,
                digest: digest.map_err(|error| problem(&error))?,
                executable: false,
                writable: false,
            });
        }
        let stdin = match &task.stdin {
            Some(text) => Some(
                self.runner
                    .import_bytes(text.as_bytes())
                    .map_err(|error| problem(&error))?,
            ),
            None => None,
        };
        let request = request(task, image, inputs, stdin);
        let result = self
            .runner
            .execute(&request)
            .map_err(|error| problem(&error))?;
        if self.verbose {
            let state = if result.reused { "cached" } else { "done" };
            println!("[{state}] {}", task.name);
        }
        let text = |digest: &Option<Digest>| match digest {
            Some(digest) => self
                .runner
                .read_content(digest)
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .map_err(|error| problem(&error)),
            None => Ok(String::new()),
        };
        let mut outcome = Outcome {
            id: task.id,
            exit_code: result.exit_code.unwrap_or(-1),
            stdout: text(&result.stdout)?,
            stderr: text(&result.stderr)?,
            ..Outcome::default()
        };
        for file in &result.files {
            let location = self
                .runner
                .local_path(&file.location)
                .unwrap_or_else(|| PathBuf::from(&file.location));
            outcome.files.push(OutputFile {
                path: file.path.clone(),
                location,
                directory: file.directory,
            });
        }
        Ok(outcome)
    }
}

/// The task-model request of a task whose image is pinned if possible and whose inputs and
/// standard input are in the store. Results are shared across runs only when the image is
/// pinned to a digest.
fn request(
    task: &Task,
    image: Option<String>,
    inputs: Vec<InputFile>,
    stdin: Option<Digest>,
) -> task_model::TaskRequest {
    let environment: BTreeMap<String, EnvironmentValue> = task
        .environment
        .iter()
        .map(|(name, value)| (name.clone(), EnvironmentValue::Literal(value.clone())))
        .collect();
    let spec = TaskSpec {
        format: FORMAT,
        namespace: String::new(),
        image: image.map(|reference| Image {
            reference,
            platform: None,
        }),
        arguments: task.command.clone(),
        environment,
        inputs,
        outputs: task
            .outputs
            .iter()
            .map(|pattern| OutputRule {
                pattern: pattern.clone(),
            })
            .collect(),
        stdin,
    };
    let scope = if spec.image_pinned() {
        ReuseScope::Shared
    } else {
        ReuseScope::Lineage
    };
    let resources = &task.resources;
    task_model::TaskRequest {
        identity: task.name.clone(),
        display: task.name.clone(),
        scope,
        attempt: 1,
        spec,
        placement: Placement {
            cpus: resources.cpus.map_or(1, |cpus| cpus.ceil().max(1.0) as u32),
            memory_bytes: resources.memory_bytes,
            disk_bytes: resources.disk_bytes,
            time_seconds: resources.time_seconds,
            ..Placement::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use workflow_graph::Resources;

    #[test]
    fn requests_share_results_when_the_image_is_pinned() {
        let task = Task {
            name: "P (1)".into(),
            command: vec!["/bin/bash".into(), "-ue".into(), ".command.sh".into()],
            environment: [("A".to_owned(), "1".to_owned())].into(),
            outputs: vec!["out.txt".into()],
            resources: Resources {
                cpus: Some(2.0),
                ..Resources::default()
            },
            ..Task::default()
        };
        let input = InputFile {
            path: ".command.sh".into(),
            kind: FileKind::File,
            digest: Digest::sha256(b"echo"),
            executable: false,
            writable: false,
        };
        let tagged = request(
            &task,
            Some("debian:stable-slim".into()),
            vec![input.clone()],
            None,
        );
        assert_eq!(tagged.scope, ReuseScope::Lineage);
        assert_eq!(tagged.spec.arguments, task.command);
        assert_eq!(tagged.placement.cpus, 2);
        assert_eq!(
            tagged.spec.environment["A"],
            EnvironmentValue::Literal("1".into())
        );
        let pinned = request(&task, Some("debian@sha256:ab".into()), vec![input], None);
        assert_eq!(pinned.scope, ReuseScope::Shared);
    }
}
