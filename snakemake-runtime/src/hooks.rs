//! Runs lifecycle shell commands through the local container executor without task caching.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use executor::{Backend, ContainerEngine, Job, LocalContainer, Pool, Staged, Staging};
use python_evaluator::{CommandOutcome, Error};
use task_model::{Image, Placement, TaskSpec};
use workflow_graph::Task;

pub fn run(
    task: &Task,
    engine: &str,
    journals: Option<&Path>,
    scratch: &Path,
    offline: bool,
    cancel: &executor::CancellationToken,
) -> Result<CommandOutcome, Error> {
    let runtime =
        workflow_graph::runtime().map_err(|error| Error::runtime_error(error.to_string()))?;
    let result = runtime.block_on(async {
        let engine = ContainerEngine::named(Some(engine).filter(|engine| !engine.is_empty()))
            .await
            .map_err(|error| {
                format!("Lifecycle hook requires a local Docker or Podman engine: {error}")
            })?;
        let engine = match journals {
            Some(root) => engine.with_journal_root(root.to_owned()),
            None => engine,
        };
        let backend = LocalContainer::new(engine, scratch.join("hooks"), Pool::new(1, 1 << 30));
        let name = format!("uwe-hook-{}-{}", std::process::id(), task.id);
        let image = task
            .image
            .as_deref()
            .unwrap_or(crate::commands::DEFAULT_IMAGE);
        let pinned = backend
            .pin(image)
            .await
            .map_err(|error| error.to_string())?;
        let spec = TaskSpec {
            output_manifest: None,
            format: task_model::FORMAT,
            namespace: String::new(),
            image: Some(Image {
                reference: pinned.unwrap_or_else(|| image.into()),
                platform: None,
            }),
            arguments: task.command.clone(),
            workdir: task.workdir.clone(),
            environment: Default::default(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            stdin: None,
            containers: Vec::new(),
            replaces_entrypoint: true,
            run_as: None,
        };
        let mut staging = Staging::default();
        for folder in [Path::new(task.workdir.as_deref().unwrap_or("/")), scratch] {
            staging.inputs.push(Staged {
                path: folder.to_string_lossy().into_owned(),
                source: folder.into(),
                directory: true,
                in_place: true,
                ..Staged::default()
            });
        }
        let placement = Placement {
            cpus: 1,
            memory_bytes: Some(1 << 30),
            offline,
            ..Placement::default()
        };
        let finished = backend
            .run(
                Job {
                    name: &name,
                    spec: &spec,
                    staging: &staging,
                    placement: &placement,
                },
                cancel,
            )
            .await
            .map_err(|error| error.to_string())?;
        let outcome = CommandOutcome {
            exit_code: finished.exit_code.unwrap_or(1),
            stdout: std::fs::read(finished.stdout()).map_err(|error| error.to_string())?,
            stderr: std::fs::read(finished.stderr()).map_err(|error| error.to_string())?,
        };
        let _ = std::fs::remove_dir_all(&finished.root);
        Ok::<_, String>(outcome)
    });
    result.map_err(|error| Error::runtime_error(format!("Lifecycle hook container: {error}")))
}
