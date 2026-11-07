//! The Computations and Software of nf-fairscape's crate: the run, its tasks and processes.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_evaluator::value::Value;
use nextflow_parser::ast::Item;

use super::files::Dataset;
use super::{Builder, COMPUTATION, SOFTWARE, file_name, refs, texts, time, uri};
use crate::observer::{Process, Task};
use crate::report_format::{Json, NEXTFLOW_VERSION};

impl Builder<'_> {
    pub(super) fn task_id(&self, task: &Task) -> String {
        let name = task.record["name"].as_str().unwrap_or_default();
        self.ark("computation", name, &task.hash)
    }

    /// The qualified name of the process of a task, with the ARK of its Software.
    pub(super) fn process(&self, task: &Task) -> (String, String) {
        let process = task.record["process"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let script = self.definition(task).and_then(|found| found.script.clone());
        let seed = format!("{}#{process}", uri(script.as_deref().unwrap_or("")));
        let software = self.ark("software", &process, &seed);
        (process, software)
    }

    pub(super) fn definition(&self, task: &Task) -> Option<&Process> {
        let id = task.process_id?;
        self.start.processes.iter().find(|process| process.id == id)
    }

    pub(super) fn run_computation(&self, inputs: &[String], generated: &[String]) -> Json {
        let parameters: Vec<Json> = (self.start.params.as_object().into_iter().flatten())
            .map(|(name, value)| Json::from(format!("{name}: {}", groovy_text(value))))
            .collect();
        Json::Object(Vec::new())
            .with("@id", self.run.as_str())
            .with("@type", texts(&COMPUTATION))
            .with("name", format!("Nextflow workflow run of '{}'", self.name))
            .with(
                "description",
                format!(
                    "Execution of the Nextflow workflow '{}' (session {})",
                    self.name, self.start.session_id
                ),
            )
            .with("runBy", self.author.as_str())
            .with("dateCreated", time(self.start.start))
            .with("command", self.start.command_line.as_str())
            .with("parameter", Json::List(parameters))
            .with(
                "usedSoftware",
                refs(&[self.workflow.clone(), self.engine.clone()]),
            )
            .with("usedDataset", refs(inputs))
            .with("generated", refs(generated))
            .with("startTime", time(self.start.start))
            .with("endTime", time(self.end.end))
            .with("nextflowVersion", NEXTFLOW_VERSION)
            .with("identifier", self.start.session_id.as_str())
    }

    pub(super) fn computation(
        &self,
        task: &Task,
        software: &str,
        used: Vec<String>,
        made: &[&Dataset],
    ) -> Json {
        let id = self.task_id(task);
        let name = task.record["name"].as_str().unwrap_or_default();
        let process = task.record["process"].as_str().unwrap_or_default();
        let generated: Vec<String> = (made.iter())
            .filter(|dataset| dataset.producers.contains(&id))
            .map(|dataset| dataset.id.clone())
            .collect();
        let mut computation = Json::Object(Vec::new())
            .with("@id", id.as_str())
            .with("@type", texts(&COMPUTATION))
            .with("name", name)
            .with(
                "description",
                format!("Nextflow task '{name}' (process {process})"),
            )
            .with("runBy", self.author.as_str())
            .with("dateCreated", time(self.start.start))
            .with(
                "command",
                script(task.record["script"].as_str().unwrap_or_default()),
            )
            .with("usedSoftware", refs(&[software.to_owned()]))
            .with("usedDataset", refs(&used))
            .with("generated", refs(&generated))
            .with("isPartOf", refs(std::slice::from_ref(&self.run)))
            .with("identifier", task.hash.as_str());
        if let Some(image) = task.record["container"].as_str() {
            computation.insert("containerImage", image);
        }
        computation
    }

    pub(super) fn workflow_software(&self) -> Json {
        let name = file_name(&self.start.script);
        let url = match self.settings.workflow {
            true => format!("workflow/{name}"),
            false => uri(&self.start.script),
        };
        let mut software = Json::Object(Vec::new())
            .with("@id", self.workflow.as_str())
            .with("@type", texts(&SOFTWARE))
            .with("name", self.name.as_str())
            .with("author", self.author.as_str())
            .with(
                "description",
                format!("Nextflow workflow script for the run '{}'", self.name),
            )
            .with("format", "nextflow")
            .with("version", self.version())
            .with("contentUrl", url);
        if let Some(repository) = &self.start.repository {
            software.insert("codeRepository", repository.as_str());
        }
        software
    }

    /// uwe, which ran the workflow, where the plugin names Nextflow.
    pub(super) fn engine_software(&self) -> Json {
        Json::Object(Vec::new())
            .with("@id", self.engine.as_str())
            .with("@type", texts(&SOFTWARE))
            .with("name", "uwe")
            .with("author", "The uwe Contributors")
            .with(
                "description",
                format!("uwe, which runs Nextflow {NEXTFLOW_VERSION} workflows without a JVM"),
            )
            .with("format", "application/x-executable")
            .with("version", env!("CARGO_PKG_VERSION"))
    }

    pub(super) fn process_software(&self, process: &str, id: &str, tasks: &[Task]) -> Json {
        let first = tasks.iter().find(|task| task.record["process"] == process);
        let found = first.and_then(|task| self.definition(task));
        let script = found
            .and_then(|found| found.script.clone())
            .unwrap_or_default();
        let definition = found.map_or(process, |found| found.definition.as_str());
        let description = template(&script, definition)
            .filter(|text| text.chars().count() >= 10)
            .unwrap_or_else(|| format!("Nextflow process '{process}' of the run '{}'", self.name));
        let version = (self.settings.tool_versions)
            .then(|| tool_versions(process, tasks))
            .flatten()
            .unwrap_or_else(|| self.version());
        let url = match self.settings.workflow && script == self.start.script {
            true => format!("workflow/{}", file_name(&script)),
            false => uri(&script),
        };
        Json::Object(Vec::new())
            .with("@id", id)
            .with("@type", texts(&SOFTWARE))
            .with("name", process)
            .with("author", self.author.as_str())
            .with("description", description)
            .with("format", "nextflow")
            .with("version", version)
            .with("contentUrl", url)
            .with("keywords", texts(&self.settings.keywords))
            .with("isPartOf", refs(std::slice::from_ref(&self.workflow)))
    }
}

/// The version of a process from the `versions.yml` files of its tasks: the version of its one
/// tool, or `tool version` for each tool, by the bare then the qualified process name.
fn tool_versions(process: &str, tasks: &[Task]) -> Option<String> {
    let bare = process.rsplit(':').next().unwrap_or(process);
    let files = tasks.iter().flat_map(|task| &task.outputs);
    let files = files.filter(|file| file_name(&file.name) == "versions.yml");
    for file in files {
        let Ok(text) = std::fs::read_to_string(&file.path) else {
            continue;
        };
        let Ok(Value::Map(map)) = nextflow_evaluator::interpreter::parse_yaml(&text) else {
            continue;
        };
        let map = map.borrow();
        let found = [bare, process].into_iter().find_map(|key| {
            let mut entries = map.entries.iter();
            entries
                .find(|(name, _)| name.0.to_string() == key)
                .map(|(_, tools)| tools.clone())
        });
        let Some(Value::Map(tools)) = found else {
            continue;
        };
        let tools: Vec<(String, String)> = (tools.borrow().entries.iter())
            .map(|(tool, version)| (tool.0.to_string(), version.to_string()))
            .collect();
        return match tools.as_slice() {
            [(_, version)] => Some(version.clone()),
            _ => Some(
                tools
                    .iter()
                    .map(|(tool, version)| format!("{tool} {version}"))
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
        };
    }
    None
}

/// The script of a process as written in its file, each line trimmed, as its description.
fn template(script: &str, definition: &str) -> Option<String> {
    let source = std::fs::read_to_string(script).ok()?;
    let parsed = nextflow_parser::parse_script(&source);
    let process = parsed.tree.items.iter().find_map(|item| match item {
        Item::Process(process) if process.name.text == definition => Some(process),
        _ => None,
    })?;
    let statement = process.body.as_ref()?.block.statements.last()?;
    let text = source.get(statement.span.start as usize..statement.span.end as usize)?;
    let quotes = ["\"\"\"", "'''", "\"", "'"];
    let inner = (quotes.iter())
        .find_map(|quote| text.strip_prefix(quote)?.strip_suffix(quote))
        .unwrap_or(text);
    let lines: Vec<&str> = inner.lines().map(str::trim).collect();
    Some(lines.join("\n").trim().to_owned())
}

/// The script of a task without the interpreter line that uwe adds, like `#!/bin/bash -ue`.
fn script(text: &str) -> &str {
    let text = text.trim();
    match text.starts_with("#!") {
        true => text.split_once('\n').map_or("", |(_, rest)| rest).trim(),
        false => text,
    }
}

/// A param value as Groovy prints it: text as it is, lists as `[a, b]`, maps as `[k:v]`.
fn groovy_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(items) => {
            let items: Vec<String> = items.iter().map(groovy_text).collect();
            format!("[{}]", items.join(", "))
        }
        serde_json::Value::Object(entries) => {
            let entries: Vec<String> = (entries.iter())
                .map(|(key, value)| format!("{key}:{}", groovy_text(value)))
                .collect();
            format!("[{}]", entries.join(", "))
        }
        other => other.to_string(),
    }
}
