//! Leaf templates as tasks: what a pod would run, and what its outcome makes.
//!
//! A container template is one task with the container's image, command and arguments. A
//! script template runs its command on the source, which the task gets as the file
//! `/argo/staging/script`, as Argo's executor writes it. A container set gives one task per
//! container, ordered by their dependencies. Resource and data templates run Argo's own
//! executor image (`argoexec resource` and `argoexec data`), as Argo does in a pod; a resource
//! template needs a Kubernetes cluster when it really runs. An HTTP template runs curl, which
//! writes the response body and status code to files that decide the success condition.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use argo_parser::model::{Container, Template};
use argo_parser::{Node, Value as Yaml};
use workflow_graph::{Input, Outcome, Task, TaskId};

use crate::http::{
    CURL_IMAGE, RESPONSE_BODY, RESPONSE_STATUS, Verdict, http_script, http_verdict, secret_headers,
};
use crate::node::{ArtifactValue, Location, Phase};
use crate::resources::{duration_seconds, resources_of};
use crate::run::{Failure, Result, Run};
use crate::value::Value;

/// The image of Argo's executor, which runs resource and data templates.
pub const ARGOEXEC_IMAGE: &str = "quay.io/argoproj/argoexec:v4.1.4";

/// Where Argo's executor puts the source of a script template.
pub const SCRIPT_PATH: &str = "/argo/staging/script";

impl Run {
    /// Asks for the tasks of a leaf template.
    pub fn start_pod(&mut self, index: usize) -> Result<()> {
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
            if let Some(input) = input_of(artifact) {
                inputs.push(input);
            }
        }
        let outputs = output_paths(template);
        let time = template
            .active_deadline_seconds
            .as_ref()
            .and_then(|seconds| seconds.0.trim().parse::<u64>().ok())
            .or_else(|| {
                template
                    .timeout
                    .as_ref()
                    .and_then(|timeout| duration_seconds(&timeout.0))
            });
        let base = |image: &str, container: Option<&Container>| {
            let mut resources = container
                .and_then(|container| container.resources.as_ref())
                .map(resources_of)
                .unwrap_or_default();
            resources.time_seconds = time;
            Task {
                id: TaskId(0),
                name: name.clone(),
                step: template.name.clone(),
                image: (!image.is_empty()).then(|| image.to_owned()),
                inputs: inputs.clone(),
                resources,
                after: inbound.clone(),
                ..Task::default()
            }
        };
        let mut tasks: Vec<Task> = Vec::new();
        let mut dependencies: Vec<Vec<usize>> = Vec::new();
        if let Some(container) = &template.container {
            let mut task = base(&container.image, Some(container));
            fill_container(&mut task, container);
            task.environment = self.environment(container, &fields)?;
            task.outputs = outputs.clone();
            self.mount_volumes(index, 0, template, &container.volume_mounts, &mut task)?;
            tasks.push(task);
        } else if let Some(script) = &template.script {
            let mut task = base(&script.image, Some(script));
            fill_container(&mut task, script);
            task.environment = self.environment(script, &fields)?;
            task.command = script.command.iter().chain(&script.args).cloned().collect();
            task.command.push(SCRIPT_PATH.to_owned());
            let source = script.source.clone().unwrap_or_default();
            task.inputs.push(Input {
                path: SCRIPT_PATH.to_owned(),
                source: String::new(),
                directory: false,
                content: Some(source.clone()),
            });
            task.script = Some(source);
            task.outputs = outputs.clone();
            self.mount_volumes(index, 0, template, &script.volume_mounts, &mut task)?;
            tasks.push(task);
        } else if let Some(set) = &template.container_set {
            let names: Vec<&str> = set
                .containers
                .iter()
                .map(|container| container.name.as_str())
                .collect();
            let order = container_order(&set.containers).map_err(Failure::error)?;
            let main = main_container(&set.containers);
            for &position in &order {
                let container = &set.containers[position];
                let mut task = base(&container.image, Some(container));
                task.name = format!("{name} ({})", container.name);
                fill_container(&mut task, container);
                task.environment = self.environment(container, &fields)?;
                if position == main {
                    task.outputs = outputs.clone();
                }
                let mounts: Vec<Node> = set
                    .volume_mounts
                    .iter()
                    .chain(&container.volume_mounts)
                    .cloned()
                    .collect();
                let slot = tasks.len();
                self.mount_volumes(index, slot, template, &mounts, &mut task)?;
                let after: Vec<usize> = container
                    .dependencies
                    .iter()
                    .filter_map(|dependency| {
                        order.iter().position(|&slot| names[slot] == dependency)
                    })
                    .collect();
                dependencies.push(after);
                tasks.push(task);
            }
        } else if let Some(resource) = &template.resource {
            self.warn(format!(
                "resource template {} runs `argoexec resource {}`, which needs a Kubernetes cluster when it really runs",
                template.name, resource.action
            ));
            let mut task = base(ARGOEXEC_IMAGE, None);
            task.command = vec![
                "argoexec".into(),
                "resource".into(),
                resource.action.clone(),
            ];
            task.command.extend(resource.flags.iter().cloned());
            task.inputs.push(Input {
                path: "/tmp/manifest.yaml".into(),
                source: String::new(),
                directory: false,
                content: Some(resource.manifest.clone()),
            });
            task.outputs = outputs.clone();
            tasks.push(task);
        } else if let Some(http) = &template.http {
            let mut task = base(CURL_IMAGE, None);
            let mut secrets = BTreeMap::new();
            for (header, reference) in secret_headers(http) {
                let what = format!("header {header} of HTTP template {}", template.name);
                secrets.insert(header, self.secret(&what, &reference)?);
            }
            let script = http_script(http, &secrets);
            task.command = vec!["sh".into(), "-c".into(), script.clone()];
            task.script = Some(script);
            task.outputs = vec![RESPONSE_BODY.to_owned(), RESPONSE_STATUS.to_owned()];
            tasks.push(task);
        } else if let Some(data) = &template.data {
            self.warn(format!(
                "data template {} runs `argoexec data`, which reads its artifact repository when it really runs",
                template.name
            ));
            let mut task = base(ARGOEXEC_IMAGE, None);
            let description = processed
                .node
                .get("data")
                .map(node_json)
                .unwrap_or_else(|| format!("{data:?}"));
            task.command = vec!["argoexec".into(), "data".into(), description];
            task.outputs = outputs.clone();
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
        let outcomes: Vec<Outcome> = self.nodes[index]
            .outcomes
            .iter()
            .flatten()
            .cloned()
            .collect();
        let template = &processed.template;
        // The main container of a container set gives the result; its tasks are in
        // dependency order.
        let slot = match &template.container_set {
            Some(set) => container_order(&set.containers)
                .ok()
                .and_then(|order| {
                    order
                        .iter()
                        .position(|&position| position == main_container(&set.containers))
                })
                .unwrap_or(0),
            None => 0,
        };
        let Some(main) = outcomes.get(slot).or(outcomes.first()).cloned() else {
            return Ok(());
        };
        let exit_code = outcomes
            .iter()
            .map(|outcome| outcome.exit_code)
            .find(|code| *code != 0)
            .unwrap_or(0);
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
        // differently, like literal braces a mock reads as a glob, by their position.
        let asked: Vec<String> = self.emitted_outputs(index, slot);
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
                        let mut text = String::from_utf8_lossy(
                            &std::fs::read(&found.location).unwrap_or_default(),
                        )
                        .into_owned();
                        if text.ends_with('\n') {
                            text.pop();
                        }
                        Some(text)
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
                location: Some(Location::File(found.location.clone())),
                optional: artifact.optional,
                directory: found.directory,
            };
            // A later task that reads the same location reads this output.
            if let Ok(declared) = self.artifact_value(artifact)
                && let Some(Location::Url(url)) = declared.location
            {
                self.store.insert(url, found.location.clone());
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
        self.keep_written(index);
        if exit_code != 0 {
            self.fail(
                index,
                Phase::Failed,
                format!("Error (exit code {exit_code})"),
            );
            return Ok(());
        }
        if let Some(error) = errors.into_iter().next() {
            self.fail(index, Phase::Error, error);
            return Ok(());
        }
        if let Some(http) = &template.http {
            let read = |path: &str| {
                file(path)
                    .map(|found| {
                        String::from_utf8_lossy(&std::fs::read(&found.location).unwrap_or_default())
                            .into_owned()
                    })
                    .unwrap_or_default()
            };
            let body = read(RESPONSE_BODY);
            self.nodes[index].outputs.result = Some(body.clone());
            match http_verdict(http, &body, &read(RESPONSE_STATUS)) {
                Verdict::Unknown => self.warn(format!(
                    "HTTP template {}: the response status is not known, as in a mock run; its success condition is not checked",
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

    /// Warns once about fields of a template and its containers that only matter to
    /// Kubernetes, which a run without Kubernetes leaves out (decision 18).
    fn warn_pod_fields(&mut self, processed: &crate::run::Processed) {
        self.warn_left_out(&processed.node);
        let node = &processed.node;
        for kind in ["container", "script"] {
            if let Some(container) = node.get(kind) {
                self.warn_left_out(container);
            }
        }
        if let Some(Node {
            value: Yaml::Sequence(containers),
            ..
        }) = node
            .get("containerSet")
            .and_then(|set| set.get("containers"))
        {
            for container in containers {
                self.warn_left_out(container);
            }
        }
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

/// The script of a command like `sh -c '...'`, which the mock executor reads.
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

/// The container of a set whose outcome is the node's: `main`, or the last.
fn main_container(containers: &[Container]) -> usize {
    containers
        .iter()
        .position(|container| container.name == "main")
        .unwrap_or(containers.len().saturating_sub(1))
}

/// The containers of a container set in an order where dependencies come first.
fn container_order(containers: &[Container]) -> std::result::Result<Vec<usize>, String> {
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

/// The input of a task for an artifact.
fn input_of(artifact: &ArtifactValue) -> Option<Input> {
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
        },
        Location::File(file) => Input {
            path,
            source: file.to_string_lossy().into_owned(),
            directory: artifact.directory || file.is_dir(),
            content: None,
        },
        Location::Url(url) => Input {
            path,
            source: url.clone(),
            directory: artifact.directory,
            content: None,
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
