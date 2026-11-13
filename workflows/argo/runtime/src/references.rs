//! Checks that simple tags name variables that exist, as Argo's `resolveAllVariables` does.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeSet;
use std::sync::OnceLock;

use argo_parser::model::{Template, WorkflowSpec};
use argo_parser::{Node, Value as Yaml};
use regex::Regex;

/// Tags with these prefixes must resolve; Argo leaves other tags alone.
const CHECKED: [&str; 7] = [
    "item.",
    "steps.",
    "inputs.",
    "outputs.",
    "pod.",
    "workflow.",
    "tasks.",
];

/// Tags that Argo accepts without a variable, as they resolve at run time.
const LATE: [&str; 6] = [
    "outputs.",
    "workflow.creationTimestamp",
    "workflow.scheduledTime",
    "workflow.duration",
    "tasks.name",
    "steps.name",
];

/// The variables that tags of one place may use, with the wildcards of Argo's scope.
#[derive(Clone, Debug, Default)]
pub(crate) struct Scope {
    names: BTreeSet<String>,
    any_item: bool,
    any_parameter: bool,
    any_artifact: bool,
}

/// The global variables of a workflow, and whether parameters may come from a later Workflow.
#[derive(Clone, Debug, Default)]
pub(crate) struct Globals {
    pub names: BTreeSet<String>,
    pub template_check: bool,
    /// Exit handlers may name any global output artifact when a `globalName` is a tag.
    pub any_artifact: bool,
}

impl Globals {
    /// The globals of Argo's `ValidateWorkflow`; `metadata` is the document's metadata.
    pub fn of_spec(spec: &WorkflowSpec, metadata: Option<&Node>, template_check: bool) -> Self {
        let mut names: BTreeSet<String> = [
            "workflow.name",
            "workflow.namespace",
            "workflow.mainEntrypoint",
            "workflow.serviceAccountName",
            "workflow.uid",
            "workflow.status",
            "workflow.annotations",
            "workflow.annotations.json",
            "workflow.labels",
            "workflow.labels.json",
        ]
        .map(str::to_owned)
        .into();
        if !spec.arguments.parameters.is_empty() {
            names.insert("workflow.parameters".to_owned());
            names.insert("workflow.parameters.json".to_owned());
        }
        for parameter in &spec.arguments.parameters {
            if !parameter.name.is_empty() {
                names.insert(format!("workflow.parameters.{}", parameter.name));
            }
        }
        let sources = [metadata, spec.workflow_metadata.as_ref()];
        for (field, kind) in [
            ("annotations", "annotations"),
            ("labels", "labels"),
            ("labelsFrom", "labels"),
        ] {
            for source in sources.iter().flatten() {
                if let Some(Yaml::Mapping(entries)) = source.get(field).map(|node| &node.value) {
                    for entry in entries {
                        names.insert(format!("workflow.{kind}.{}", entry.key));
                    }
                }
            }
        }
        if spec.priority.is_some() {
            names.insert("workflow.priority".to_owned());
        }
        Globals {
            names,
            template_check,
            any_artifact: false,
        }
    }

    /// Readies the globals for the exit handler, as Argo v4 does before checking it.
    pub fn exit_handler(&mut self, templates: &[Template]) {
        self.names.insert("workflow.failures".to_owned());
        self.any_artifact |= (templates
            .iter()
            .flat_map(|template| &template.outputs.artifacts))
        .any(|artifact| is_parameter(&artifact.global_name));
    }
}

impl Scope {
    pub fn insert(&mut self, name: impl Into<String>) {
        self.names.insert(name.into());
    }

    /// The scope a template gives its own body, as Argo's `validateInputs` and
    /// `validateTemplate` build it.
    pub fn of_template(template: &Template) -> Self {
        let mut scope = Scope::default();
        let inputs = &template.inputs;
        for parameter in &inputs.parameters {
            scope.insert(format!("inputs.parameters.{}", parameter.name));
        }
        if !inputs.parameters.is_empty() {
            scope.insert("inputs.parameters");
        }
        let leaf = is_leaf(template);
        for artifact in &inputs.artifacts {
            scope.insert(format!("inputs.artifacts.{}", artifact.name));
            if leaf {
                scope.insert(format!("inputs.artifacts.{}.path", artifact.name));
            }
        }
        if is_pod(template) {
            scope.insert("pod.name");
        }
        if leaf {
            for artifact in &template.outputs.artifacts {
                if !artifact.path.is_empty() {
                    scope.insert(format!("outputs.artifacts.{}.path", artifact.name));
                }
            }
            for parameter in &template.outputs.parameters {
                if parameter
                    .value_from
                    .as_ref()
                    .is_some_and(|from| !from.path.is_empty())
                {
                    scope.insert(format!("outputs.parameters.{}.path", parameter.name));
                }
            }
        }
        scope
    }

    /// Adds the item of a loop, as Argo's `addItemsToScope`.
    pub fn add_items(&mut self, items: &[Node], param: &str, sequence: bool) {
        if !items.is_empty() {
            for item in items {
                match &item.value {
                    Yaml::Sequence(list) => {
                        for index in 0..list.len() {
                            self.insert(format!("item.[{index}]"));
                        }
                    }
                    Yaml::Mapping(entries) => {
                        for entry in entries {
                            self.insert(format!("item.{}", entry.key));
                        }
                    }
                    _ => self.insert("item"),
                }
            }
        } else if !param.is_empty() {
            self.insert("item");
            self.any_item = true;
        } else if sequence {
            self.insert("item");
        }
    }

    /// Adds what a finished step or task gives, as Argo's `addOutputsToScope`. Global output
    /// names go to `globals`, as they become workflow variables.
    pub fn add_outputs(
        &mut self,
        called: Option<&Template>,
        prefix: &str,
        call: Call,
        globals: &mut Globals,
    ) {
        for field in ["id", "startedAt", "finishedAt", "hostNodeName"] {
            self.insert(format!("{prefix}.{field}"));
        }
        let Some(template) = called else {
            return;
        };
        if template.daemon == Some(true) {
            self.insert(format!("{prefix}.ip"));
        }
        if has_output(template) {
            self.insert(format!("{prefix}.outputs.result"));
            self.insert(format!("{prefix}.exitCode"));
        }
        for parameter in &template.outputs.parameters {
            self.insert(format!("{prefix}.outputs.parameters.{}", parameter.name));
            self.add_global("parameters", &parameter.global_name, globals);
        }
        for artifact in &template.outputs.artifacts {
            self.insert(format!("{prefix}.outputs.artifacts.{}", artifact.name));
            self.add_global("artifacts", &artifact.global_name, globals);
        }
        if call.aggregate {
            if template.script.is_some() || template.container_set.is_some() {
                self.insert(format!("{prefix}.outputs.result"));
                self.insert(format!("{prefix}.exitCode"));
            }
            self.insert(format!("{prefix}.outputs.parameters"));
        }
        if call.ancestor {
            self.insert(format!("{prefix}.status"));
        }
    }

    /// Adds the workflow output of a `globalName`; a name that is a tag allows any.
    fn add_global(&mut self, kind: &str, global: &str, globals: &mut Globals) {
        if global.is_empty() {
            return;
        }
        if is_parameter(global) {
            match kind {
                "parameters" => self.any_parameter = true,
                _ => self.any_artifact = true,
            }
            return;
        }
        let name = format!("workflow.outputs.{kind}.{global}");
        globals.names.insert(name.clone());
        self.insert(name);
    }

    /// Checks every tag in the strings and keys of a value tree.
    pub fn resolve_node(&self, node: &Node, globals: &Globals) -> Result<(), String> {
        match &node.value {
            Yaml::String(text) => self.resolve(text, globals),
            Yaml::Sequence(items) => items
                .iter()
                .try_for_each(|item| self.resolve_node(item, globals)),
            Yaml::Mapping(entries) => entries.iter().try_for_each(|entry| {
                self.resolve(&entry.key, globals)?;
                self.resolve_node(&entry.value, globals)
            }),
            _ => Ok(()),
        }
    }

    /// Checks the simple tags of a text; expression tags are not checked, as in Argo.
    pub fn resolve(&self, text: &str, globals: &Globals) -> Result<(), String> {
        let mut rest = text;
        while let Some(start) = rest.find("{{") {
            let after = &rest[start + 2..];
            let Some(end) = after.find("}}") else {
                break;
            };
            let tag = &after[..end];
            rest = &after[end + 2..];
            if !tag.starts_with('=') && !self.allows(tag.trim(), globals) {
                return Err(format!("failed to resolve {{{{{tag}}}}}"));
            }
        }
        Ok(())
    }

    fn allows(&self, tag: &str, globals: &Globals) -> bool {
        !CHECKED.iter().any(|prefix| tag.starts_with(prefix))
            || self.names.contains(tag)
            || globals.names.contains(tag)
            || ((tag == "item" || tag.starts_with("item.")) && self.any_item)
            || (tag.starts_with("workflow.outputs.parameters.") && self.any_parameter)
            || (tag.starts_with("workflow.outputs.artifacts.")
                && (self.any_artifact || globals.any_artifact))
            || LATE.iter().any(|prefix| tag.starts_with(prefix))
            || tag.starts_with("node.name")
            || (tag.starts_with("workflow.parameters") && globals.template_check)
    }
}

/// How a step or task calls its template, for [`Scope::add_outputs`].
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Call {
    /// The call loops, so its outputs are lists.
    pub aggregate: bool,
    /// The call is an ancestor of the task whose tags are checked.
    pub ancestor: bool,
}

/// Argo's `IsLeaf`: a template that does work itself.
fn is_leaf(template: &Template) -> bool {
    is_pod(template)
        || (template.steps.is_empty()
            && template.dag.is_none()
            && (template.http.is_some() || template.plugin.is_some()))
}

/// Argo's `IsPodType`, by the first type that `GetType` finds.
fn is_pod(template: &Template) -> bool {
    if template.container.is_some() || template.container_set.is_some() {
        return true;
    }
    if !template.steps.is_empty() || template.dag.is_some() {
        return false;
    }
    template.script.is_some() || template.resource.is_some() || template.data.is_some()
}

/// Argo's `HasOutput`: the template gives a result and an exit code.
fn has_output(template: &Template) -> bool {
    let main = template.container_set.as_ref().is_some_and(|set| {
        set.containers
            .iter()
            .any(|container| container.name == "main")
    });
    template.container.is_some()
        || main
        || template.script.is_some()
        || template.data.is_some()
        || template.http.is_some()
        || template.plugin.is_some()
}

/// Whether a global name is itself a tag, which Argo resolves only at run time.
fn is_parameter(name: &str) -> bool {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    let pattern = PATTERN.get_or_init(|| {
        Regex::new(r"\{\{[-a-zA-Z0-9]+(\.[-a-zA-Z0-9_]+)*\}\}").expect("a valid pattern")
    });
    pattern.is_match(name)
}

#[cfg(test)]
#[path = "references_tests.rs"]
mod tests;
