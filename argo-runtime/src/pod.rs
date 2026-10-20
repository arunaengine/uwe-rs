//! Lowers leaf templates to tasks and interprets their outcomes.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use argo_parser::model::{Container, Template};
use argo_parser::{Node, Value as Yaml, decode};
use workflow_graph::{
    Container as TaskContainer, Input, Outcome, Resources, RunAs, Task, TaskId, Writable,
};

use crate::http::{
    CURL_IMAGE, RESPONSE_BODY, RESPONSE_HEADERS, RESPONSE_STATUS, Verdict, http_script,
    http_verdict, secret_headers,
};
use crate::node::{ArtifactValue, Location, Phase};
use crate::resources::{duration_seconds, resources_of};
use crate::run::{Failure, Result, Run, scalar_text};
use crate::value::Value;

/// Where Argo's executor puts the source of a script template.
pub const SCRIPT_PATH: &str = "/argo/staging/script";

impl Run {
    /// Asks for the tasks of a leaf template.
    pub fn start_pod(&mut self, index: usize) -> Result<()> {
        if !self.admit(index) {
            return Ok(());
        }
        let Some(processed) = self.nodes[index].processed.clone() else {
            return Ok(());
        };
        let template = &processed.template;
        self.warn_pod_fields(&processed);
        let fields = self.pod_fields(index);
        let name = self.nodes[index].name.clone();
        let inbound = self.nodes[index].inbound.clone();
        let mut inputs: Vec<Input> = Vec::new();
        for artifact in &processed.artifacts {
            let declared =
                (template.inputs.artifacts.iter()).find(|declared| declared.name == artifact.name);
            if let Some(input) = self.input_artifact(artifact, declared)? {
                inputs.push(input);
            }
        }
        let outputs = output_paths(template);
        let time = template
            .active_deadline_seconds
            .as_ref()
            .map(|seconds| {
                seconds
                    .0
                    .trim()
                    .parse::<u64>()
                    .ok()
                    .filter(|seconds| *seconds > 0)
                    .ok_or_else(|| {
                        Failure::error("activeDeadlineSeconds must be a positive integer")
                    })
            })
            .transpose()?
            .or_else(|| {
                template
                    .timeout
                    .as_ref()
                    .and_then(|timeout| duration_seconds(&timeout.0))
            });
        let time = self.nodes[index]
            .retry_limit
            .map(|deadline| {
                let remaining = deadline.saturating_duration_since(self.clock);
                remaining
                    .as_secs()
                    .saturating_add(u64::from(remaining.subsec_nanos() > 0))
            })
            .or(time);
        let base = |image: &str, container: Option<&Container>| -> Result<Task> {
            let mut resources = container
                .and_then(|container| container.resources.as_ref())
                .map(resources_of)
                .transpose()
                .map_err(resource_error)?
                .unwrap_or_default();
            resources.time_seconds = time;
            Ok(Task {
                id: TaskId(0),
                name: name.clone(),
                step: template.name.clone(),
                image: (!image.is_empty()).then(|| image.to_owned()),
                inputs: inputs.clone(),
                resources,
                after: inbound.clone(),
                ..Task::default()
            })
        };
        // Init containers run one after the other before the main containers, each as a task
        // of its own on the same volumes, so that the next task gets the files it wrote.
        let init = init_containers(template)?;
        let done = self.nodes[index].tasks.clone();
        if done.len() < init.len() {
            let (container, mirror) = &init[done.len()];
            let mut task = base(&container.image, Some(container))?;
            task.name = format!("{name} (init {})", container.name);
            task.inputs.clear();
            fill_container(&mut task, container);
            task.run_as = Some(self.run_as(template, Some(container)));
            task.environment = self.environment(container, &fields)?;
            if let Some(&last) = done.last() {
                task.after = vec![last];
            }
            let mut mounts = container.volume_mounts.clone();
            if *mirror {
                for mount in main_mounts(template) {
                    let path = |mount: &Node| mount.get("mountPath").map(scalar_text);
                    if !mounts.iter().any(|known| path(known) == path(&mount)) {
                        mounts.push(mount);
                    }
                }
            }
            self.mount_volumes(index, done.len(), template, &mounts, &mut task)?;
            task.id = self.next_task_id()?;
            self.ask(index, task);
            return Ok(());
        }
        let first = init.len();
        let mut tasks: Vec<Task> = Vec::new();
        let mut dependencies: Vec<Vec<usize>> = Vec::new();
        if let Some(container) = &template.container {
            let mut task = base(&container.image, Some(container))?;
            fill_container(&mut task, container);
            task.run_as = Some(self.run_as(template, Some(container)));
            task.environment = self.environment(container, &fields)?;
            task.outputs = outputs.clone();
            self.mount_volumes(index, first, template, &container.volume_mounts, &mut task)?;
            tasks.push(task);
        } else if let Some(script) = &template.script {
            let mut task = base(&script.image, Some(script))?;
            fill_container(&mut task, script);
            task.run_as = Some(self.run_as(template, Some(script)));
            task.environment = self.environment(script, &fields)?;
            task.command = script.command.iter().chain(&script.args).cloned().collect();
            task.command.push(SCRIPT_PATH.to_owned());
            let source = script.source.clone().unwrap_or_default();
            task.inputs.push(Input {
                path: SCRIPT_PATH.to_owned(),
                source: String::new(),
                directory: false,
                content: Some(source.clone()),
                // Argo's executor writes the script with mode 0755, so that a command like
                // `sh -c` may run it directly.
                executable: true,
                ..Default::default()
            });
            task.script = Some(source);
            task.outputs = outputs.clone();
            self.mount_volumes(index, first, template, &script.volume_mounts, &mut task)?;
            tasks.push(task);
        } else if let Some(set) = &template.container_set {
            // One task runs every container of the set, as one pod does: the main container
            // first, then the others in the order of the set. They share the volumes that any
            // of them mounts, and the task asks for the sum of their resources, as Argo does.
            container_order(&set.containers).map_err(Failure::error)?;
            let main = main_container(&set.containers);
            let attempts = retry_attempts(set.retry_strategy.as_ref())?;
            let positions =
                std::iter::once(main).chain((0..set.containers.len()).filter(|&at| at != main));
            let mut task = base(&set.containers[main].image, None)?;
            let mut mounts: Vec<Node> = set.volume_mounts.clone();
            for position in positions {
                let container = &set.containers[position];
                let environment = self.environment(container, &fields)?;
                if position == main {
                    fill_container(&mut task, container);
                    task.environment = environment.clone();
                    task.run_as = Some(self.run_as(template, Some(container)));
                }
                task.resources = sum(&task.resources, container)?;
                let path = |mount: &Node| mount.get("mountPath").map(scalar_text);
                for mount in &container.volume_mounts {
                    if !mounts.iter().any(|known| path(known) == path(mount)) {
                        mounts.push(mount.clone());
                    }
                }
                task.containers.push(TaskContainer {
                    name: container.name.clone(),
                    image: (!container.image.is_empty()).then(|| container.image.clone()),
                    command: (container.command.iter())
                        .chain(&container.args)
                        .cloned()
                        .collect(),
                    workdir: (!container.working_dir.is_empty())
                        .then(|| container.working_dir.clone()),
                    environment,
                    after: container.dependencies.clone(),
                    attempts,
                    replaces_entrypoint: !container.command.is_empty(),
                });
            }
            task.outputs = outputs.clone();
            self.mount_volumes(index, first, template, &mounts, &mut task)?;
            tasks.push(task);
        } else if let Some(http) = &template.http {
            let mut task = base(CURL_IMAGE, None)?;
            let mut secrets = BTreeMap::new();
            for (header, reference) in secret_headers(http) {
                let what = format!("header {header} of HTTP template {}", template.name);
                secrets.insert(header, self.secret(&what, &reference)?);
            }
            let script = http_script(http, &secrets).map_err(Failure::error)?;
            task.command = vec!["sh".into(), "-c".into(), script.clone()];
            task.script = Some(script);
            task.outputs = vec![
                RESPONSE_BODY.to_owned(),
                RESPONSE_STATUS.to_owned(),
                RESPONSE_HEADERS.to_owned(),
            ];
            tasks.push(task);
        }
        if dependencies.len() < tasks.len() {
            dependencies.resize(tasks.len(), Vec::new());
        }
        let mut ids: Vec<TaskId> = Vec::new();
        for _ in &tasks {
            ids.push(self.next_task_id()?);
        }
        for (slot, mut task) in tasks.into_iter().enumerate() {
            task.id = ids[slot];
            for &id in &done {
                if !task.after.contains(&id) {
                    task.after.push(id);
                }
            }
            for &dependency in &dependencies[slot] {
                if let Some(&id) = ids.get(dependency)
                    && !task.after.contains(&id)
                {
                    task.after.push(id);
                }
            }
            self.ask(index, task);
        }
        self.changed = true;
        if self.nodes[index].tasks.is_empty() {
            self.set_phase(index, Phase::Succeeded, String::new());
        }
        Ok(())
    }

    /// Takes the outcomes of a leaf's tasks: its phase, result, exit code and outputs.
    pub fn finish_pod(&mut self, index: usize) -> Result<()> {
        let Some(processed) = self.nodes[index].processed.clone() else {
            return Ok(());
        };
        let template = &processed.template;
        // The outcomes of the main containers, after those of the init containers.
        let first = init_containers(template)?.len();
        let outcomes: Vec<Outcome> = self.nodes[index]
            .outcomes
            .iter()
            .skip(first)
            .flatten()
            .cloned()
            .collect();
        // One task runs the main container, or all containers of a container set.
        let slot = 0;
        let Some(main) = outcomes.first().cloned() else {
            return Ok(());
        };
        if main.error.is_some() {
            self.fail(index, Phase::Error, main.stderr);
            return Ok(());
        }
        // The exit code of the main container; a container set fails when any of its
        // containers failed, with the message of the first by name, as Argo sorts them.
        let own = |outcome: &Outcome| match outcome.containers.first() {
            Some(container) => container.exit_code.unwrap_or(outcome.exit_code),
            None => outcome.exit_code,
        };
        let exit_code = own(&main);
        let mut failed: Vec<(&str, i32)> = (main.containers.iter())
            .filter_map(|container| {
                let code = container.exit_code.unwrap_or(main.exit_code);
                (code != 0).then_some((container.name.as_str(), code))
            })
            .collect();
        failed.sort();
        let failure = match failed.first() {
            Some((container, code)) => Some(format!("{container}: Error (exit code {code})")),
            None if main.exit_code != 0 => Some(format!("Error (exit code {})", main.exit_code)),
            None => None,
        };
        let mut result = main.stdout.clone();
        if result.ends_with('\n') {
            result.pop();
        }
        let mut outputs = crate::node::NodeOutputs {
            result: Some(result.clone()),
            exit_code: Some(exit_code.to_string()),
            ..Default::default()
        };
        // Output files by the path the task asked for; paths the executor spelled
        // differently, like literal braces the simulator reads as a glob, by their position.
        let asked: Vec<String> = self.emitted_outputs(index, first + slot);
        let file = |path: &str| {
            let clean = |text: &str| {
                text.trim_start_matches('/')
                    .trim_end_matches('/')
                    .to_owned()
            };
            let wanted = clean(path);
            let files: Vec<&workflow_graph::OutputFile> =
                outcomes.iter().flat_map(|outcome| &outcome.files).collect();
            files
                .iter()
                .find(|file| clean(&file.path) == wanted)
                .copied()
                .or_else(|| {
                    let position = asked.iter().position(|output| clean(output) == wanted)?;
                    (files.len() == asked.len()).then(|| files[position])
                })
        };
        let mut errors: Vec<String> = Vec::new();
        for parameter in &template.outputs.parameters {
            let from = parameter.value_from.as_ref();
            let default = from
                .and_then(|from| from.default.clone())
                .or_else(|| parameter.default.clone());
            let value = if let Some(from) = from.filter(|from| !from.path.is_empty()) {
                match file(&from.path) {
                    Some(found) if !found.directory => {
                        let mut text = match self.read_output(&found.location) {
                            Ok(text) => text,
                            Err(error) => {
                                errors.push(format!(
                                    "cannot read output parameter {}: {error}",
                                    parameter.name
                                ));
                                continue;
                            }
                        };
                        if text.ends_with('\n') {
                            text.pop();
                        }
                        // The simulator makes up empty files for outputs that it cannot
                        // simulate, so a simulated run takes the default of such an output.
                        let made_up = text.is_empty() && self.options.assume_inputs;
                        (!made_up || default.is_none()).then_some(text)
                    }
                    _ => None,
                }
            } else if let Some(from) =
                from.filter(|from| !from.json_path.is_empty() || !from.jq_filter.is_empty())
            {
                let _ = from;
                if default.is_none() {
                    self.warn(format!(
                        "output parameter {} of template {} reads the Kubernetes resource; it is empty",
                        parameter.name, template.name
                    ));
                    Some(String::new())
                } else {
                    None
                }
            } else {
                parameter.value.clone().map(|value| value.0)
            };
            let value = match (value, default) {
                (Some(value), _) => value,
                (None, Some(default)) => default.0,
                (None, None) if exit_code != 0 => continue,
                (None, None) => {
                    errors.push(format!("output parameter {} has no value", parameter.name));
                    continue;
                }
            };
            if !parameter.global_name.is_empty() {
                self.globals.insert(
                    format!("workflow.outputs.parameters.{}", parameter.global_name),
                    value.clone(),
                );
            }
            outputs.parameters.push((parameter.name.clone(), value));
        }
        for artifact in &template.outputs.artifacts {
            let found = file(&artifact.path);
            let Some(found) = found else {
                if !artifact.optional && exit_code == 0 {
                    errors.push(format!(
                        "output artifact {}: {} was not made",
                        artifact.name, artifact.path
                    ));
                }
                continue;
            };
            let value = ArtifactValue {
                name: artifact.name.clone(),
                path: artifact.path.clone(),
                location: Some(Location::File(found.location.clone().into())),
                optional: artifact.optional,
                directory: found.directory,
            };
            // A later task that reads the same location reads this output.
            let plain = argo_parser::model::Artifact {
                archive: None,
                ..artifact.clone()
            };
            let archived = self.archive_output(index, artifact, &found.location)?;
            if artifact
                .archive
                .as_ref()
                .is_some_and(|node| node.get("none").is_some())
            {
                self.raw_outputs.insert(found.location.clone().into());
            }
            // The address alone, so that a later write of the same key wins, as in Argo.
            if let Ok(Some(url)) = crate::inputs::artifact_url(&plain) {
                self.store
                    .insert(url.clone(), found.location.clone().into());
                if artifact.s3.is_some() && !url.starts_with(crate::inputs::REPOSITORY) {
                    self.artifact_delivery(url, archived);
                }
            }
            if !artifact.global_name.is_empty() {
                self.global_artifacts.insert(
                    format!("workflow.outputs.artifacts.{}", artifact.global_name),
                    value.clone(),
                );
            }
            outputs.artifacts.push(value);
        }
        self.nodes[index].outputs = outputs;
        if let Err(message) = self.keep_written(index) {
            self.fail(index, Phase::Error, message);
            return Ok(());
        }
        if let Some(message) = failure {
            self.fail(index, Phase::Failed, message);
            return Ok(());
        }
        if let Some(error) = errors.into_iter().next() {
            self.fail(index, Phase::Error, error);
            return Ok(());
        }
        if let Some(http) = &template.http {
            let files = self.files.clone();
            let read = |path: &str| {
                file(path)
                    .map(|found| crate::run::output_text(files.as_deref(), &found.location))
                    .unwrap_or_else(|| Ok(String::new()))
            };
            let (body, status, headers) = match (
                read(RESPONSE_BODY),
                read(RESPONSE_STATUS),
                read(RESPONSE_HEADERS),
            ) {
                (Ok(body), Ok(status), Ok(headers)) => (body, status, headers),
                (Err(error), _, _) | (_, Err(error), _) | (_, _, Err(error)) => {
                    self.fail(
                        index,
                        Phase::Error,
                        format!("cannot read HTTP response: {error}"),
                    );
                    return Ok(());
                }
            };
            self.nodes[index].outputs.result = Some(body.clone());
            match http_verdict(http, &body, &status, &headers) {
                Verdict::Unknown if !self.options.assume_inputs => {
                    self.fail(index, Phase::Error, "HTTP response status is not known".to_owned());
                    return Ok(());
                }
                Verdict::Unknown => self.warn(format!(
                    "HTTP template {}: the response status is not known, as in a simulated run; its success condition is not checked",
                    template.name
                )),
                Verdict::Succeeded => {}
                Verdict::Failed(message) => {
                    self.fail(index, Phase::Failed, message);
                    return Ok(());
                }
                Verdict::Error(message) => {
                    self.fail(index, Phase::Error, message);
                    return Ok(());
                }
            }
        }
        self.set_phase(index, Phase::Succeeded, String::new());
        Ok(())
    }

    /// Whether a leaf still runs its init containers: its tasks so far are all init containers.
    pub fn initializing(&self, index: usize) -> bool {
        let node = &self.nodes[index];
        node.kind == crate::node::NodeKind::Pod
            && node.processed.as_ref().is_some_and(|processed| {
                init_containers(&processed.template)
                    .is_ok_and(|init| node.tasks.len() <= init.len())
            })
    }

    /// Takes the outcome of an init container: its files go into the volumes it wrote, and
    /// the next init container or the main containers start. One that fails fails the node,
    /// as its pod fails in Argo.
    pub fn finish_init(&mut self, index: usize) -> Result<()> {
        let Some(Some(outcome)) = self.nodes[index].outcomes.last().cloned() else {
            return Ok(());
        };
        if let Err(message) = self.keep_written(index) {
            self.fail(index, Phase::Error, message);
            return Ok(());
        }
        if outcome.error.is_some() {
            self.fail(index, Phase::Error, outcome.stderr);
            return Ok(());
        }
        if outcome.exit_code != 0 {
            let node = &self.nodes[index];
            let container = match &node.processed {
                Some(processed) => init_containers(&processed.template)?
                    .into_iter()
                    .nth(node.tasks.len() - 1)
                    .map(|(container, _)| container.name)
                    .unwrap_or_default(),
                None => String::new(),
            };
            let message = format!("{container}: Error (exit code {})", outcome.exit_code);
            self.fail(index, Phase::Error, message);
            return Ok(());
        }
        self.start_pod(index)
    }

    /// The output paths the main task of a leaf asked for: its outputs, then its mounts.
    fn emitted_outputs(&self, index: usize, slot: usize) -> Vec<String> {
        let node = &self.nodes[index];
        let mut paths = match &node.processed {
            Some(processed) => output_paths(&processed.template),
            None => Vec::new(),
        };
        for mount in node.written.iter().filter(|mount| mount.slot == slot) {
            paths.push(format!("{}/", mount.path.trim_end_matches('/')));
        }
        paths
    }

    /// The fields of a node's pod that environment variables read with `fieldRef`, as the run
    /// makes them up: the pod's name, namespace and uid, its service account, node and IP.
    pub(crate) fn pod_fields(&self, index: usize) -> BTreeMap<&'static str, String> {
        let node = &self.nodes[index];
        let pod = self.pod_name(&node.name, &node.template);
        let global = |name: &str| self.globals.get(name).cloned().unwrap_or_default();
        let account = match global("workflow.serviceAccountName") {
            account if account.is_empty() => "default".to_owned(),
            account => account,
        };
        BTreeMap::from([
            ("metadata.uid", crate::run::uid(&pod)),
            ("metadata.name", pod),
            ("metadata.namespace", global("workflow.namespace")),
            ("spec.serviceAccountName", account),
            ("spec.nodeName", "iunctus".to_owned()),
            ("status.podIP", crate::run::pod_ip(&node.name)),
            ("status.hostIP", "127.0.0.1".to_owned()),
        ])
    }

    /// Selects user and group from container, template, then workflow securityContext; missing
    /// IDs defer to the image.
    fn run_as(&self, template: &Template, container: Option<&Container>) -> RunAs {
        let contexts = [
            container.and_then(|container| container.security_context.as_ref()),
            template.security_context.as_ref(),
            self.spec.security_context.as_ref(),
        ];
        let id = |field: &str| {
            (contexts.iter().flatten())
                .find_map(|context| context.get(field))
                .and_then(|value| scalar_text(value).parse::<u32>().ok())
        };
        RunAs {
            user: id("runAsUser"),
            group: id("runAsGroup"),
        }
    }

    fn warn_pod_fields(&mut self, processed: &crate::run::Processed) {
        self.warn_left_out(&processed.node);
        let template = &processed.node;
        for kind in ["container", "script"] {
            if let Some(container) = template.get(kind) {
                self.warn_left_out(container);
            }
        }
        let set = template
            .get("containerSet")
            .and_then(|set| set.get("containers"));
        for list in [set, template.get("initContainers")].into_iter().flatten() {
            if let Yaml::Sequence(containers) = &list.value {
                for container in containers {
                    self.warn_left_out(container);
                }
            }
        }
    }
}

/// The init containers of a template that runs containers, each with whether it mirrors the
/// volume mounts of the main container (`mirrorVolumeMounts`).
fn init_containers(template: &Template) -> Result<Vec<(Container, bool)>> {
    if template.container.is_none() && template.script.is_none() && template.container_set.is_none()
    {
        return Ok(Vec::new());
    }
    let mut containers = Vec::new();
    for node in &template.init_containers {
        let (container, _) = decode::<Container>(node).map_err(|error| {
            Failure::error(format!(
                "init container of template {}: {error}",
                template.name
            ))
        })?;
        if let Some(reason) = crate::checks::cluster_only(&Template {
            name: template.name.clone(),
            container: Some(container.clone()),
            ..Template::default()
        }) {
            return Err(Failure::unsupported(reason));
        }
        let mirror = matches!(
            node.get("mirrorVolumeMounts").map(|mirror| &mirror.value),
            Some(Yaml::Bool(true))
        );
        containers.push((container, mirror));
    }
    Ok(containers)
}

/// The volume mounts of a template's main container, which init containers can mirror.
fn main_mounts(template: &Template) -> Vec<Node> {
    if let Some(container) = template.container.as_ref().or(template.script.as_ref()) {
        return container.volume_mounts.clone();
    }
    match &template.container_set {
        Some(set) => set.volume_mounts.clone(),
        None => Vec::new(),
    }
}

/// Fills a task from a container: one command line of `command` and `args` (decision 18), and
/// the working directory.
pub(crate) fn fill_container(task: &mut Task, container: &Container) {
    task.command = container
        .command
        .iter()
        .chain(&container.args)
        .cloned()
        .collect();
    task.script = shell_script(&task.command);
    // A container's `command` replaces the entrypoint of the image, as in Kubernetes; `args`
    // alone run through it.
    task.replaces_entrypoint = !container.command.is_empty();
    if !container.working_dir.is_empty() {
        task.workdir = Some(container.working_dir.clone());
    }
}

/// The files of a leaf's outputs: artifact paths and the paths of output parameters, without
/// repeats, deeper paths first, so that a directory output does not keep a file below it from
/// being made.
fn output_paths(template: &Template) -> Vec<String> {
    let mut outputs: Vec<String> = Vec::new();
    for artifact in &template.outputs.artifacts {
        if !artifact.path.is_empty() && !outputs.contains(&artifact.path) {
            outputs.push(artifact.path.clone());
        }
    }
    for parameter in &template.outputs.parameters {
        if let Some(from) = &parameter.value_from
            && !from.path.is_empty()
            && !outputs.contains(&from.path)
        {
            outputs.push(from.path.clone());
        }
    }
    outputs.sort_by_key(|path| std::cmp::Reverse(path.trim_end_matches('/').matches('/').count()));
    outputs
}

/// The script of a command like `sh -c '...'`, which the simulator reads.
fn shell_script(command: &[String]) -> Option<String> {
    let program = command.first()?;
    let program = program.rsplit('/').next().unwrap_or(program);
    if !matches!(program, "sh" | "bash" | "ash" | "zsh" | "dash") {
        return None;
    }
    let flag = command[1..]
        .iter()
        .position(|argument| argument.starts_with('-') && argument.contains('c'))?;
    command.get(flag + 2).cloned()
}

/// How often each container of a set runs its command at most while it fails: the `retries`
/// of the set's `retryStrategy`, which Argo's executor takes as the number of tries, else once.
fn retry_attempts(strategy: Option<&Node>) -> Result<u32> {
    let retries = strategy
        .and_then(|strategy| strategy.get("retries"))
        .map(scalar_text)
        .map(|text| {
            text.trim()
                .parse::<u32>()
                .ok()
                .filter(|count| *count > 0)
                .ok_or_else(|| {
                    Failure::error("containerSet.retryStrategy.retries must be a positive integer")
                })
        })
        .transpose()?;
    Ok(retries.unwrap_or(1))
}

pub(crate) fn resource_error(message: String) -> Failure {
    match message.strip_prefix("unsupported: ") {
        Some(message) => Failure::unsupported(message),
        None => Failure::error(message),
    }
}

/// The resources of a pod with one more container: the sum of their requests.
fn sum(total: &Resources, container: &Container) -> Result<Resources> {
    let more = (container.resources.as_ref())
        .map(resources_of)
        .transpose()
        .map_err(resource_error)?
        .unwrap_or_default();
    let add = |a: Option<u64>, b: Option<u64>| match (a, b) {
        (Some(a), Some(b)) => a
            .checked_add(b)
            .map(Some)
            .ok_or_else(|| Failure::error("container set resource sum overflows")),
        (a, b) => Ok(a.or(b)),
    };
    let cpus = match (total.cpus, more.cpus) {
        (Some(a), Some(b)) => Some(a + b),
        (a, b) => a.or(b),
    };
    if cpus.is_some_and(|cpus| !cpus.is_finite()) {
        return Err(Failure::error("container set cpu resource sum overflows"));
    }
    let gpus = add(total.gpus.map(u64::from), more.gpus.map(u64::from))?
        .map(u32::try_from)
        .transpose()
        .map_err(|_| Failure::error("container set gpu resource sum overflows"))?;
    Ok(Resources {
        cpus,
        memory_bytes: add(total.memory_bytes, more.memory_bytes)?,
        disk_bytes: add(total.disk_bytes, more.disk_bytes)?,
        time_seconds: total.time_seconds,
        gpus,
    })
}

/// The container of a set whose outcome is the node's: `main`, or the last.
fn main_container(containers: &[Container]) -> usize {
    containers
        .iter()
        .position(|container| container.name == "main")
        .unwrap_or(containers.len().saturating_sub(1))
}

/// The containers of a container set in an order where dependencies come first.
fn container_order(containers: &[Container]) -> std::result::Result<Vec<usize>, String> {
    if containers.is_empty() {
        return Err("containerSet must contain at least one container".into());
    }
    let mut order: Vec<usize> = Vec::new();
    while order.len() < containers.len() {
        let next = (0..containers.len()).find(|&position| {
            !order.contains(&position)
                && containers[position].dependencies.iter().all(|dependency| {
                    order
                        .iter()
                        .any(|&done| &containers[done].name == dependency)
                })
        });
        match next {
            Some(position) => order.push(position),
            None => {
                return Err(
                    "the dependencies of the container set form a cycle or name unknown containers"
                        .into(),
                );
            }
        }
    }
    Ok(order)
}

/// The input of a task for an artifact. The task gets a copy that it may change, as Argo's
/// executor writes each input artifact into the pod.
pub(crate) fn input_of(artifact: &ArtifactValue) -> Option<Input> {
    let location = artifact.location.as_ref()?;
    let path = if artifact.path.is_empty() {
        format!("/tmp/{}", artifact.name)
    } else {
        artifact.path.clone()
    };
    Some(match location {
        Location::Raw(data) => Input {
            path,
            source: String::new(),
            directory: false,
            content: Some(data.clone()),
            ..Default::default()
        },
        Location::File(file) => Input {
            executable: false,
            path,
            source: file.to_string_lossy().into_owned(),
            directory: artifact.directory || file.is_dir(),
            content: None,
            writable: Writable::Copy,
        },
        Location::Url(url) => Input {
            executable: false,
            path,
            source: url.clone(),
            directory: artifact.directory,
            content: None,
            writable: Writable::Copy,
        },
    })
}

/// A value tree as JSON text.
pub(crate) fn node_json(node: &Node) -> String {
    fn value(node: &Node) -> Value {
        match &node.value {
            Yaml::Null => Value::Nil,
            Yaml::Bool(flag) => Value::Bool(*flag),
            Yaml::Integer(number) => Value::Int(*number),
            Yaml::Unsigned(number) => Value::Float(*number as f64),
            Yaml::Float(number) => Value::Float(*number),
            Yaml::String(text) => Value::String(text.clone()),
            Yaml::Sequence(items) => Value::Array(items.iter().map(value).collect()),
            Yaml::Mapping(entries) => Value::Map(
                entries
                    .iter()
                    .map(|entry| (entry.key.clone(), value(&entry.value)))
                    .collect(),
            ),
        }
    }
    value(node).to_json()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_shell_scripts() {
        let command = |words: &[&str]| {
            words
                .iter()
                .map(|word| (*word).to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            shell_script(&command(&["sh", "-c", "echo hi > out.txt"])),
            Some("echo hi > out.txt".to_owned())
        );
        assert_eq!(
            shell_script(&command(&["/bin/bash", "-euxc", "ls"])),
            Some("ls".to_owned())
        );
        assert_eq!(shell_script(&command(&["python", "-c", "x"])), None);
    }
}
