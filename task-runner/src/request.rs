//! Turns a task that a driver asked for into a task request: the hashed specification with its
//! inputs in the store, and the identity of the task in its run.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use executor::Backend;
use task_model::{
    Accelerator, ContainerSpec, Digest, EnvironmentValue, FORMAT, Image, InputFile, OutputRule,
    Placement, ReuseScope, TaskRequest, TaskSpec,
};
use workflow_graph::{Network, Reuse, Task, Writable};

use crate::inputs::Inputs;
use crate::runner::{Prepared, segment};
use crate::{Runner, RunnerError};

/// The longest step name that an identity keeps, so that it stays a short path segment.
const STEP_LENGTH: usize = 100;
/// Separates results made without network access from otherwise identical online results.
const OFFLINE_NAMESPACE: &str = "network-denied";

impl<B: Backend> Runner<B> {
    /// The request of a task whose image is `image`, pinned to a digest where possible, for
    /// its first attempt. The standard input and literal inputs go into the store; local
    /// inputs are hashed, and stored too when the runner imports inputs.
    pub async fn request(
        &self,
        task: &Task,
        image: Option<String>,
    ) -> Result<Prepared, RunnerError> {
        let admission = self.admit().await?;
        let inputs = self.inputs();
        self.phase(None, Some(&admission), None, prepare(&inputs, task, image))
            .await
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
    let (siblings, layout) = inputs.siblings(&task.inputs).await?;
    let folders: Vec<std::path::PathBuf> = (task.inputs.iter())
        .filter(|input| input.writable == Writable::Shared)
        .filter_map(|input| std::fs::canonicalize(&input.source).ok())
        .collect();
    for input in &task.inputs {
        let workdir = task.workdir.as_deref();
        let (file, source) = match crate::inputs::present(input, &folders, workdir) {
            true => inputs.present(input).await?,
            false => inputs.input(input, &siblings).await?,
        };
        files.push(file);
        sources.push(source);
    }
    let stdin = match &task.stdin {
        Some(text) => Some(inputs.bytes(text.as_bytes()).await?),
        None => None,
    };
    let mut request = request(task, image, files, stdin);
    if let Some(layout) = layout {
        request.spec.namespace = format!("{}siblings-{}", request.spec.namespace, layout.hex());
    }
    // A task that changes an input where it is has effects that no stored result has; a
    // task of a shared folder is answered by its declared inputs and outputs.
    if (task.inputs.iter()).any(|input| input.writable == Writable::InPlace) {
        request.scope = ReuseScope::None;
    }
    Ok(Prepared {
        request,
        sources,
        step: task.step.clone(),
        per_container: 1,
        reuse: matches!(task.reuse, Reuse::Allowed | Reuse::Lineage),
        snapshots: inputs.snapshots.clone(),
    })
}

/// The request of a task whose image is pinned if possible and whose inputs and standard input
/// are in the store.
pub fn request(
    task: &Task,
    image: Option<String>,
    inputs: Vec<InputFile>,
    stdin: Option<Digest>,
) -> TaskRequest {
    let literal = |environment: &BTreeMap<String, String>| -> BTreeMap<String, EnvironmentValue> {
        (environment.iter())
            .map(|(name, value)| (name.clone(), EnvironmentValue::Literal(value.clone())))
            .collect()
    };
    let environment = literal(&task.environment);
    let image = image.map(|reference| Image {
        reference,
        platform: None,
    });
    let containers = (task.containers.iter().enumerate())
        .map(|(position, container)| ContainerSpec {
            name: container.name.clone(),
            image: match position {
                0 => image.clone(),
                _ => (container.image.clone()).map(|reference| Image {
                    reference,
                    platform: None,
                }),
            },
            arguments: container.command.clone(),
            workdir: container.workdir.clone(),
            environment: literal(&container.environment),
            after: container.after.clone(),
            attempts: container.attempts.max(1),
            replaces_entrypoint: container.replaces_entrypoint,
            service: container.service,
        })
        .collect();
    let spec = TaskSpec {
        format: FORMAT,
        namespace: match task.network {
            Network::Denied => OFFLINE_NAMESPACE.to_owned(),
            Network::Allowed | Network::Unstated => String::new(),
        },
        image,
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
        output_manifest: task.output_manifest.clone(),
        containers,
        replaces_entrypoint: task.replaces_entrypoint,
        run_as: (task.run_as).map(|run_as| task_model::RunAs {
            user: run_as.user,
            group: run_as.group,
        }),
    };
    let scope = if task.reuse == Reuse::Disabled {
        ReuseScope::None
    } else if task.reuse == Reuse::Lineage {
        ReuseScope::Lineage
    } else if spec.image_pinned() && task.network == Network::Denied {
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
            offline: task.network == Network::Denied,
            queue: resources.queue.clone(),
            labels: resources.labels.clone(),
            options: resources.options.clone(),
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
    fn pinned_shares_results() {
        let task = Task {
            id: TaskId(12),
            name: "P (1)".into(),
            step: "NFCORE:P".into(),
            network: Network::Denied,
            command: vec!["/bin/bash".into(), "-ue".into(), ".command.sh".into()],
            workdir: Some("/work".into()),
            environment: [("A".to_owned(), "1".to_owned())].into(),
            outputs: vec!["out.txt".into(), "db/".into()],
            output_manifest: Some("cwl.output.json".into()),
            resources: Resources {
                cpus: Some(2.0),
                gpus: Some(1),
                queue: Some("long".into()),
                labels: [("team".to_owned(), "genomics".to_owned())].into(),
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
        assert_eq!(tagged.spec.output_manifest, task.output_manifest);
        assert_eq!(tagged.spec.stdin, stdin);
        assert_eq!(tagged.placement.cpus, 2);
        assert_eq!(tagged.placement.accelerators[0].count, 1);
        assert_eq!(tagged.placement.queue.as_deref(), Some("long"));
        assert_eq!(tagged.placement.labels["team"], "genomics");
        assert_eq!(
            tagged.spec.environment["A"],
            EnvironmentValue::Literal("1".into())
        );
        let pinned = request(&task, Some("debian@sha256:ab".into()), vec![input], None);
        assert_eq!(pinned.scope, ReuseScope::Shared);
    }

    #[test]
    fn separates_offline_identity() {
        let task = Task {
            command: vec!["true".into()],
            ..Task::default()
        };
        let online = request(&task, Some("image@sha256:ab".into()), Vec::new(), None);
        let mut explicitly_online = task.clone();
        explicitly_online.network = Network::Allowed;
        let explicitly_online = request(
            &explicitly_online,
            Some("image@sha256:ab".into()),
            Vec::new(),
            None,
        );
        let mut offline = task;
        offline.network = Network::Denied;
        let offline = request(&offline, Some("image@sha256:ab".into()), Vec::new(), None);

        assert!(!online.placement.offline && !explicitly_online.placement.offline);
        assert!(offline.placement.offline);
        assert!(online.spec.namespace.is_empty());
        assert_eq!(explicitly_online.spec.namespace, online.spec.namespace);
        assert_eq!(offline.spec.namespace, OFFLINE_NAMESPACE);
        assert_eq!(
            online.spec.cache_key().expect("an online key"),
            explicitly_online
                .spec
                .cache_key()
                .expect("an explicitly online key")
        );
        assert_ne!(
            online.spec.cache_key().expect("an online key"),
            offline.spec.cache_key().expect("an offline key")
        );
        assert_eq!(online.scope, ReuseScope::Lineage);
        assert_eq!(explicitly_online.scope, ReuseScope::Lineage);
        assert_eq!(offline.scope, ReuseScope::Shared);
        let resumed = Task {
            command: vec!["true".into()],
            network: Network::Denied,
            reuse: Reuse::Lineage,
            ..Task::default()
        };
        let resumed = request(&resumed, Some("image@sha256:ab".into()), Vec::new(), None);
        assert_eq!(resumed.scope, ReuseScope::Lineage, "never shared");
    }

    #[test]
    fn identifies_step_tasks() {
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
