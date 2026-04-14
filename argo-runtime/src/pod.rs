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
use workflow_graph::{Input, Outcome, Resources, Task, TaskId};

use crate::run::{ArtifactValue, Failure, Location, MAX_TASKS, Phase, Result, Run};
use crate::value::Value;

/// The image of Argo's executor, which runs resource and data templates.
pub const ARGOEXEC_IMAGE: &str = "quay.io/argoproj/argoexec:v4.1.4";

/// The image that runs HTTP templates.
pub const CURL_IMAGE: &str = "curlimages/curl:8.16.0";

/// Where Argo's executor puts the source of a script template.
pub const SCRIPT_PATH: &str = "/argo/staging/script";

/// Where the curl of an HTTP template writes the response body and status code.
const RESPONSE_BODY: &str = "/tmp/response-body";
const RESPONSE_STATUS: &str = "/tmp/response-status";

impl Run {
    /// Asks for the tasks of a leaf template.
    pub fn start_pod(&mut self, index: usize) -> Result<()> {
        let Some(processed) = self.nodes[index].processed.clone() else {
            return Ok(());
        };
        let template = &processed.template;
        self.warn_pod_fields(template);
        let name = self.nodes[index].name.clone();
        let inbound = self.nodes[index].inbound.clone();
        let mut inputs: Vec<Input> = Vec::new();
        for artifact in &processed.artifacts {
            if let Some(input) = input_of(artifact) {
                inputs.push(input);
            }
        }
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
            fill_container(&mut task, container, &mut |message| self.warn(message));
            task.outputs = outputs.clone();
            tasks.push(task);
        } else if let Some(script) = &template.script {
            let mut task = base(&script.image, Some(script));
            fill_container(&mut task, script, &mut |message| self.warn(message));
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
                fill_container(&mut task, container, &mut |message| self.warn(message));
                if position == main {
                    task.outputs = outputs.clone();
                }
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
            let mut command: Vec<String> = vec![
                "curl".into(),
                "--silent".into(),
                "--show-error".into(),
                "--location".into(),
            ];
            if http.insecure_skip_verify {
                command.push("--insecure".into());
            }
            let method = if http.method.is_empty() {
                "GET"
            } else {
                http.method.as_str()
            };
            command.extend(["--request".to_owned(), method.to_owned()]);
            for header in &http.headers {
                let name = header
                    .get("name")
                    .map(crate::run::scalar_text)
                    .unwrap_or_default();
                if header.get("valueFrom").is_some() {
                    self.warn(format!(
                        "HTTP template {}: header {name} reads a Secret, which a run without Kubernetes cannot; it is left out",
                        template.name
                    ));
                    continue;
                }
                let value = header
                    .get("value")
                    .map(crate::run::scalar_text)
                    .unwrap_or_default();
                command.extend(["--header".to_owned(), format!("{name}: {value}")]);
            }
            if !http.body.is_empty() {
                command.extend(["--data".to_owned(), http.body.clone()]);
            }
            command.extend([
                "--output".to_owned(),
                RESPONSE_BODY.to_owned(),
                "--write-out".to_owned(),
                "%{http_code}".to_owned(),
                http.url.clone(),
            ]);
            let quoted: Vec<String> = command.iter().map(|word| shell_quote(word)).collect();
            let script = format!("{} > {RESPONSE_STATUS}", quoted.join(" "));
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
            tasks.push(task);
        }
        if dependencies.len() < tasks.len() {
            dependencies.resize(tasks.len(), Vec::new());
        }
        let mut ids: Vec<TaskId> = Vec::new();
        for _ in &tasks {
            if self.next_task > MAX_TASKS {
                return Err(Failure::error(format!(
                    "the run asks for more than {MAX_TASKS} tasks; does a loop not end?"
                )));
            }
            ids.push(TaskId(self.next_task));
            self.next_task += 1;
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
            self.tasks.insert(task.id, (index, slot));
            self.nodes[index].tasks.push(task.id);
            self.nodes[index].outcomes.push(None);
            self.emitted.push(task);
        }
        self.changed = true;
        if self.nodes[index].tasks.is_empty() {
            self.set_phase(index, Phase::Succeeded, String::new());
        }
        Ok(())
    }
}
