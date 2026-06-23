//! Executor plugin templates. Argo runs them in its agent pod, whose sidecars are the servers of
//! the plugins: the agent posts the template to `/api/v1/template.execute` of one plugin after
//! the other, until one replies with a node. Here each call is a task in the image of the
//! plugin: a shell script starts the plugin's server, posts the template as the agent does, and
//! keeps the reply and its HTTP status as output files. The reply gives the node its phase,
//! message and outputs; a plugin that does not take the template passes it to the next one.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use argo_parser::model::{Container, Template};
use argo_parser::{Node, Value as Yaml, decode, parse_yaml};
use workflow_graph::{Input, Outcome, Task};

use crate::library::{Document, Kind};
use crate::node::{NodeOutputs, Phase};
use crate::pod::fill_container;
use crate::resources::resources_of;
use crate::run::{Result, Run, scalar_text, uid};
use crate::value::Value;

/// Where the task finds the request, the JSON the agent would post.
pub const REQUEST_PATH: &str = "/argo/plugin/request.json";

/// Where the task keeps the body and the HTTP status of the reply.
pub const REPLY_PATH: &str = "/argo/plugin/reply.json";
pub const STATUS_PATH: &str = "/argo/plugin/status";

/// Where Argo mounts the token that the agent sends and the plugin checks.
pub const TOKEN_PATH: &str = "/var/run/argo/token";

/// An executor plugin: its name, the container of its server, and the port it serves.
#[derive(Clone, Debug)]
pub(crate) struct Plugin {
    pub name: String,
    pub container: Container,
    pub port: String,
}

impl Plugin {
    fn new(name: String, container: Container) -> std::result::Result<Plugin, String> {
        let port = match container.ports.as_ref().map(|ports| &ports.value) {
            Some(Yaml::Sequence(ports)) => ports
                .first()
                .and_then(|port| port.get("containerPort"))
                .map(scalar_text),
            _ => None,
        };
        match port {
            Some(port) if !port.is_empty() => Ok(Plugin {
                name,
                container,
                port,
            }),
            _ => Err(format!(
                "executor plugin {name}: at least one port is mandatory"
            )),
        }
    }

    /// The plugin that a ConfigMap of the library installs, read as Argo's `FromConfigMap`
    /// reads it: the container is YAML text under `data.sidecar.container`.
    fn of_config_map(document: &Document) -> std::result::Result<Plugin, String> {
        let text = document
            .node
            .get("data")
            .and_then(|data| data.get("sidecar.container"))
            .and_then(Node::as_str)
            .unwrap_or("");
        let invalid = |message: String| {
            format!(
                "executor plugin {} in {}: {message}",
                document.name,
                document.path.display()
            )
        };
        let node = parse_yaml(text)
            .map_err(|error| invalid(error.message))?
            .ok_or_else(|| invalid("sidecar.container is empty".to_owned()))?;
        let (container, _) =
            decode::<Container>(&node).map_err(|error| invalid(error.to_string()))?;
        Plugin::new(document.name.clone(), container)
    }
}

/// The script that calls a plugin. Its arguments are the port and the command of the server.
fn call_script() -> String {
    format!(
        r#"port="$1"
shift
"$@" &
server=$!
url="http://localhost:$port/api/v1/template.execute"
token="$(cat {TOKEN_PATH})"
code=1
for attempt in $(seq 30); do
  if command -v curl > /dev/null; then
    curl --silent --output {REPLY_PATH} --write-out '%{{http_code}}' --header 'Content-Type: application/json' --header "Authorization: Bearer $token" --data-binary @{REQUEST_PATH} "$url" > {STATUS_PATH} && code=0
  elif command -v wget > /dev/null; then
    wget -q -O {REPLY_PATH} --header 'Content-Type: application/json' --header "Authorization: Bearer $token" --post-file {REQUEST_PATH} "$url" && echo 200 > {STATUS_PATH} && code=0
  else
    echo 'the image of the plugin has neither curl nor wget' >&2
    break
  fi
  if [ "$code" = 0 ]; then break; fi
  sleep 1
done
kill "$server" 2> /dev/null
exit "$code"
"#
    )
}

/// A name without case and punctuation, to match plugin names with template keys:
/// `workflow-level-hello-executor-plugin` holds `workflowLevelHello`.
fn simple(name: &str) -> String {
    name.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

impl Run {
    /// The plugins that a plugin template goes to, in the order they are tried: those of
    /// `spec.executorPlugins`, which replace the installed plugins as in Argo, or else the
    /// plugin ConfigMaps of the library. Plugins whose name holds a key of the template's
    /// `plugin` field come first, as they are the ones most likely to take it.
    pub(crate) fn plugins(&mut self, template: &Template) -> Vec<Plugin> {
        let mut plugins = Vec::new();
        let mut problems = Vec::new();
        if self.spec.executor_plugins.is_empty() {
            for document in self.library.all(Kind::ExecutorPlugin, &self.document.path) {
                match Plugin::of_config_map(&document) {
                    Ok(plugin) => plugins.push(plugin),
                    Err(problem) => problems.push(problem),
                }
            }
        } else {
            for plugin in &self.spec.executor_plugins {
                let name = plugin.metadata.name.clone();
                match Plugin::new(name, plugin.spec.sidecar.container.clone()) {
                    Ok(plugin) => plugins.push(plugin),
                    Err(problem) => problems.push(problem),
                }
            }
        }
        for problem in problems {
            self.warn(format!("{problem}; the plugin is left out"));
        }
        let keys: Vec<String> = match template.plugin.as_ref().map(|plugin| &plugin.value) {
            Some(Yaml::Mapping(entries)) => {
                entries.iter().map(|entry| simple(&entry.key)).collect()
            }
            _ => Vec::new(),
        };
        let matches = |plugin: &Plugin| {
            let name = simple(&plugin.name);
            keys.iter().any(|key| !key.is_empty() && name.contains(key))
        };
        plugins.sort_by_key(|plugin| !matches(plugin));
        plugins
    }

    /// Asks for the call of the next plugin, or ends the node when no plugin is left.
    pub(crate) fn start_plugin(&mut self, index: usize) -> Result<()> {
        let Some(processed) = self.nodes[index].processed.clone() else {
            return Ok(());
        };
        let plugins = self.plugins(&processed.template);
        let tried = self.nodes[index].tasks.len();
        let Some(plugin) = plugins.get(tried) else {
            if plugins.is_empty() {
                self.warn(
                    "no executor plugin is installed: the workflow names none in spec.executorPlugins, and the library holds no plugin ConfigMap".to_owned(),
                );
            }
            self.fail(
                index,
                Phase::Error,
                "no plugin executed the template".to_owned(),
            );
            return Ok(());
        };
        let fields = self.pod_fields(index);
        let mut task = Task {
            id: self.next_task_id()?,
            name: self.nodes[index].name.clone(),
            step: processed.template.name.clone(),
            image: Some(plugin.container.image.clone()).filter(|image| !image.is_empty()),
            resources: plugin
                .container
                .resources
                .as_ref()
                .map(resources_of)
                .unwrap_or_default(),
            after: match self.nodes[index].tasks.last() {
                Some(&previous) => vec![previous],
                None => self.nodes[index].inbound.clone(),
            },
            ..Task::default()
        };
        fill_container(&mut task, &plugin.container);
        task.environment = self.environment(&plugin.container, &fields)?;
        let script = call_script();
        task.command = vec![
            "sh".to_owned(),
            "-c".to_owned(),
            script.clone(),
            "call-plugin".to_owned(),
            plugin.port.clone(),
        ];
        task.command
            .extend(plugin.container.command.iter().cloned());
        task.command.extend(plugin.container.args.iter().cloned());
        task.script = Some(script);
        let global = |name: &str| self.globals.get(name).cloned().unwrap_or_default();
        let metadata = [
            ("name", global("workflow.name")),
            ("namespace", global("workflow.namespace")),
            ("uid", global("workflow.uid")),
        ]
        .map(|(key, value)| format!("\"{key}\":{}", Value::String(value).to_json()))
        .join(",");
        let request = format!(
            "{{\"workflow\":{{\"metadata\":{{{metadata}}}}},\"template\":{}}}",
            crate::pod::node_json(&processed.node)
        );
        for (path, content) in [
            (REQUEST_PATH, request),
            (TOKEN_PATH, uid(&format!("{}-executor-plugin", plugin.name))),
        ] {
            task.inputs.push(Input {
                path: path.to_owned(),
                source: String::new(),
                directory: false,
                content: Some(content),
            });
        }
        task.outputs = vec![REPLY_PATH.to_owned(), STATUS_PATH.to_owned()];
        self.ask(index, task);
        Ok(())
    }

    /// Takes the reply of a plugin: the node's phase, message and outputs, or the call of the
    /// next plugin when this one did not take the template.
    pub(crate) fn finish_plugin(&mut self, index: usize) -> Result<()> {
        let Some(processed) = self.nodes[index].processed.clone() else {
            return Ok(());
        };
        let Some(Some(outcome)) = self.nodes[index].outcomes.last().cloned() else {
            return Ok(());
        };
        let tried = self.nodes[index].tasks.len();
        let plugin = self
            .plugins(&processed.template)
            .get(tried.saturating_sub(1))
            .map(|plugin| plugin.name.clone())
            .unwrap_or_default();
        if outcome.exit_code != 0 {
            let message = format!(
                "executor plugin {plugin}: the call failed with exit code {}: {}",
                outcome.exit_code,
                outcome.stderr.trim()
            );
            self.fail(index, Phase::Error, message);
            return Ok(());
        }
        let status = read(&outcome, STATUS_PATH);
        let body = read(&outcome, REPLY_PATH);
        match status.trim() {
            "" => {
                self.warn(format!(
                    "executor plugin {plugin}: the reply is not known, as in a simulated run; the node succeeds"
                ));
                self.nodes[index].outputs =
                    declared_outputs(&processed.template, &mut |message| self.warn(message));
                self.set_phase(index, Phase::Succeeded, String::new());
                return Ok(());
            }
            "404" => return self.start_plugin(index),
            "200" => {}
            status => {
                let message = format!("executor plugin {plugin}: HTTP {status}: {}", body.trim());
                self.fail(index, Phase::Error, message);
                return Ok(());
            }
        }
        let reply: serde_json::Value = match serde_json::from_str(&body) {
            Ok(reply) => reply,
            Err(error) => {
                let message = format!("executor plugin {plugin}: the reply is not JSON: {error}");
                self.fail(index, Phase::Error, message);
                return Ok(());
            }
        };
        let Some(node) = reply.get("node").filter(|node| !node.is_null()) else {
            return self.start_plugin(index);
        };
        let text = |value: Option<&serde_json::Value>| match value {
            Some(serde_json::Value::String(text)) => text.clone(),
            Some(serde_json::Value::Null) | None => String::new(),
            Some(value) => value.to_string(),
        };
        let message = text(node.get("message"));
        let mut outputs = NodeOutputs::default();
        if let Some(parameters) = node
            .pointer("/outputs/parameters")
            .and_then(serde_json::Value::as_array)
        {
            for parameter in parameters {
                outputs
                    .parameters
                    .push((text(parameter.get("name")), text(parameter.get("value"))));
            }
        }
        if let Some(result) = node.pointer("/outputs/result") {
            outputs.result = Some(text(Some(result)));
        }
        self.nodes[index].outputs = outputs;
        match text(node.get("phase")).as_str() {
            "Succeeded" => self.set_phase(index, Phase::Succeeded, message),
            "Failed" => self.fail(index, Phase::Failed, message),
            "Error" => self.fail(index, Phase::Error, message),
            phase @ ("Running" | "Pending") => self.fail(
                index,
                Phase::Error,
                format!(
                    "executor plugin {plugin} replied {phase}; iunctus does not call a plugin again later"
                ),
            ),
            phase => self.fail(
                index,
                Phase::Error,
                format!("executor plugin {plugin} replied the unknown phase `{phase}`"),
            ),
        }
        Ok(())
    }
}

/// The text of an output file of a task, or nothing when the task did not make it.
fn read(outcome: &Outcome, path: &str) -> String {
    let wanted = path.trim_start_matches('/');
    outcome
        .files
        .iter()
        .find(|file| file.path.trim_start_matches('/') == wanted && !file.directory)
        .and_then(|file| std::fs::read(&file.location).ok())
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default()
}

/// The output parameters that a template declares, for a node whose reply is not known: their
/// value or default, or empty with a warning.
fn declared_outputs(template: &Template, warn: &mut dyn FnMut(String)) -> NodeOutputs {
    let mut outputs = NodeOutputs::default();
    for parameter in &template.outputs.parameters {
        let value = parameter
            .value
            .clone()
            .or_else(|| {
                parameter
                    .value_from
                    .as_ref()
                    .and_then(|from| from.default.clone())
            })
            .or_else(|| parameter.default.clone());
        let value = match value {
            Some(value) => value.0,
            None => {
                warn(format!(
                    "output parameter {} of plugin template {} comes from the plugin; it is empty",
                    parameter.name, template.name
                ));
                String::new()
            }
        };
        outputs.parameters.push((parameter.name.clone(), value));
    }
    outputs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_names_match_template_keys() {
        assert!(
            simple("workflow-level-hello-executor-plugin").contains(&simple("workflowLevelHello"))
        );
        assert!(!simple("massive").contains(&simple("hello")));
    }

    #[test]
    fn the_script_calls_the_plugin_api() {
        let script = call_script();
        assert!(script.contains("/api/v1/template.execute"));
        assert!(script.contains(&format!("@{REQUEST_PATH}")));
        assert!(script.contains(&format!("> {STATUS_PATH}")));
    }
}
