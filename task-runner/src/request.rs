//! Turns a task that a driver asked for into a task request: the hashed specification with its
//! inputs in the store, and the identity of the task in its run.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use executor::Backend;
use task_model::{
    Accelerator, Digest, EnvironmentValue, FORMAT, Image, InputFile, OutputRule, Placement,
    ReuseScope, TaskRequest, TaskSpec,
};
use workflow_graph::Task;

use crate::inputs::{Inputs, Mode};
use crate::runner::{Prepared, segment};
use crate::{Runner, RunnerError};

/// The longest step name that an identity keeps, so that it stays a short path segment.
const STEP_LENGTH: usize = 100;

impl<B: Backend> Runner<B> {
    /// The request of a task whose image is `image`, pinned to a digest where possible, for
    /// its first attempt. The standard input and literal inputs go into the store; local
    /// inputs are hashed, and stored too when the runner imports inputs.
    pub async fn request(
        &self,
        task: &Task,
        image: Option<String>,
    ) -> Result<Prepared, RunnerError> {
        let inputs = Inputs {
            store: &self.store,
            settings: &self.settings,
            mode: Mode::Run,
        };
        prepare(&inputs, task, image).await
    }
}

/// The request of a task, with the digests of its inputs as `inputs` makes them.
pub(crate) async fn prepare(
    inputs: &Inputs<'_>,
    task: &Task,
    image: Option<String>,
) -> Result<Prepared, RunnerError> {
    let mut files = Vec::new();
    let mut sources = Vec::new();
    for input in &task.inputs {
        let (file, source) = inputs.input(input).await?;
        files.push(file);
        sources.push(source);
    }
    let stdin = match &task.stdin {
        Some(text) => Some(inputs.bytes(text.as_bytes()).await?),
        None => None,
    };
    Ok(Prepared {
        request: request(task, image, files, stdin),
        sources,
    })
}

/// The request of a task whose image is pinned if possible and whose inputs and standard input
/// are in the store. Results are shared across runs only when the image is pinned to a digest.
pub fn request(
    task: &Task,
    image: Option<String>,
    inputs: Vec<InputFile>,
    stdin: Option<Digest>,
) -> TaskRequest {
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
        workdir: task.workdir.clone(),
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
    TaskRequest {
        identity: identity(task),
        display: task.name.clone(),
        scope,
        attempt: 1,
        spec,
        placement: Placement {
            cpus: resources.cpus.map_or(1, |cpus| cpus.ceil().max(1.0) as u32),
            memory_bytes: resources.memory_bytes,
            disk_bytes: resources.disk_bytes,
            time_seconds: resources.time_seconds,
            accelerators: (resources.gpus.filter(|count| *count > 0))
                .map(|count| vec![Accelerator { count, kind: None }])
                .unwrap_or_default(),
            offline: task.offline,
            ..Placement::default()
        },
    }
}

/// The identity of a task in its run, as one path segment: its step and its id, like
/// `FASTQC-12`. The id makes it unique in the run, the step makes it readable. The display name
/// is neither unique nor safe in a path.
pub fn identity(task: &Task) -> String {
    let step: String = segment(&task.step).chars().take(STEP_LENGTH).collect();
    if step.is_empty() {
        task.id.to_string()
    } else {
        format!("{step}-{}", task.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use task_model::FileKind;
    use workflow_graph::{Resources, TaskId};

    #[test]
    fn requests_share_results_when_the_image_is_pinned() {
        let task = Task {
            id: TaskId(12),
            name: "P (1)".into(),
            step: "NFCORE:P".into(),
            command: vec!["/bin/bash".into(), "-ue".into(), ".command.sh".into()],
            workdir: Some("/work".into()),
            environment: [("A".to_owned(), "1".to_owned())].into(),
            outputs: vec!["out.txt".into(), "db/".into()],
            resources: Resources {
                cpus: Some(2.0),
                gpus: Some(1),
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
        let stdin = Some(Digest::sha256(b"text"));
        let tagged = request(
            &task,
            Some("debian:stable-slim".into()),
            vec![input.clone()],
            stdin.clone(),
        );
        assert_eq!(tagged.scope, ReuseScope::Lineage);
        assert_eq!(tagged.identity, "NFCORE_P-12");
        assert_eq!(tagged.display, "P (1)");
        assert_eq!(tagged.spec.arguments, task.command);
        assert_eq!(tagged.spec.workdir.as_deref(), Some("/work"));
        assert_eq!(tagged.spec.outputs[1].pattern, "db/");
        assert_eq!(tagged.spec.stdin, stdin);
        assert_eq!(tagged.placement.cpus, 2);
        assert_eq!(tagged.placement.accelerators[0].count, 1);
        assert_eq!(
            tagged.spec.environment["A"],
            EnvironmentValue::Literal("1".into())
        );
        let pinned = request(&task, Some("debian@sha256:ab".into()), vec![input], None);
        assert_eq!(pinned.scope, ReuseScope::Shared);
    }

    #[test]
    fn identities_come_from_the_step_and_the_id() {
        let task = |step: &str, id: u64| Task {
            id: TaskId(id),
            step: step.into(),
            ..Task::default()
        };
        assert_eq!(identity(&task("a b/c", 3)), "a_b_c-3");
        assert_eq!(identity(&task("", 4)), "4");
        let long = identity(&task(&"x".repeat(300), 5));
        assert_eq!(long.len(), STEP_LENGTH + 2);
    }
}
