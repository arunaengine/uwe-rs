//! How a run starts: the spec of the workflow joined with the WorkflowTemplate it runs, the
//! name of the workflow, its global variables, and warnings about fields that need Kubernetes.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::PathBuf;

use argo_parser::model::AnyString;
use argo_parser::{Node, Value as Yaml};

use crate::library::{Document, Kind, Library};
use crate::merge::merge;
use crate::run::{
    Failure, Result, Run, empty_mapping, fnv32a, parameters_json, remove_key, scalar_text, uid,
};
use crate::tags::Variables;
use crate::time;
use crate::validate::not_found;
use crate::value::Value;

impl Run {
    pub(crate) fn set_globals(&mut self, metadata: Option<&Node>) {
        let created = self.created;
        let namespace = metadata
            .and_then(|metadata| metadata.get("namespace"))
            .and_then(Node::as_str)
            .filter(|namespace| !namespace.is_empty())
            .unwrap_or("default")
            .to_owned();
        let mut globals = BTreeMap::new();
        globals.insert("workflow.name".into(), self.workflow_name.clone());
        globals.insert("workflow.namespace".into(), namespace);
        globals.insert("workflow.mainEntrypoint".into(), self.entrypoint.clone());
        globals.insert(
            "workflow.serviceAccountName".into(),
            self.spec.service_account_name.clone(),
        );
        globals.insert("workflow.uid".into(), uid(&self.workflow_name));
        globals.insert("workflow.creationTimestamp".into(), time::rfc3339(created));
        globals.insert(
            "workflow.creationTimestamp.RFC3339".into(),
            time::rfc3339(created),
        );
        globals.insert(
            "workflow.creationTimestamp.s".into(),
            created.as_second().to_string(),
        );
        for (letter, value) in time::strftime(created) {
            globals.insert(format!("workflow.creationTimestamp.{letter}"), value);
        }
        globals.insert("workflow.duration".into(), "0".into());
        if let Some(priority) = self.spec.priority {
            globals.insert("workflow.priority".into(), priority.to_string());
        }
        if self.document.kind == Kind::CronWorkflow {
            globals.insert("workflow.scheduledTime".into(), time::rfc3339(created));
        }
        let parameters = parameters_json(&self.spec.arguments.parameters);
        globals.insert("workflow.parameters".into(), parameters.clone());
        globals.insert("workflow.parameters.json".into(), parameters);
        for parameter in &self.spec.arguments.parameters {
            let value = parameter.value.clone().unwrap_or_default().0;
            globals.insert(format!("workflow.parameters.{}", parameter.name), value);
        }
        for (field, key) in [
            ("labels", "workflow.labels"),
            ("annotations", "workflow.annotations"),
        ] {
            let mut map = BTreeMap::new();
            if let Some(Node {
                value: Yaml::Mapping(entries),
                ..
            }) = metadata.and_then(|metadata| metadata.get(field))
            {
                for entry in entries {
                    let value = scalar_text(&entry.value);
                    globals.insert(format!("{key}.{}", entry.key), value.clone());
                    map.insert(entry.key.clone(), Value::String(value));
                }
            }
            globals.insert(key.to_owned(), Value::Map(map).to_json());
        }
        // Parameter values may use other globals, like `{{workflow.name}}`.
        let variables = Variables::new(globals.clone());
        for (name, value) in globals.iter_mut() {
            if name.starts_with("workflow.parameters.")
                && value.contains("{{")
                && let Ok(resolved) = crate::tags::replace(value, &variables, true)
            {
                *value = resolved;
            }
        }
        for parameter in &mut self.spec.arguments.parameters {
            if let Some(resolved) = globals.get(&format!("workflow.parameters.{}", parameter.name))
            {
                parameter.value = Some(AnyString(resolved.clone()));
            }
        }
        self.globals = globals;
    }

    /// Warns once about fields that change what a workflow does on Kubernetes, which a run
    /// without Kubernetes leaves out.
    pub(crate) fn warn_ignored(&mut self, spec: &Node) {
        for (field, what) in [
            (
                "volumes",
                "workflow volumes are not mounted; tasks share files only as artifacts",
            ),
            (
                "volumeClaimTemplates",
                "volume claims are not created; tasks share files only as artifacts",
            ),
            (
                "synchronization",
                "synchronization is left out: one run holds no locks",
            ),
            (
                "suspend",
                "the workflow starts at once instead of suspended",
            ),
        ] {
            if spec.get(field).is_some_and(|node| node.value != Yaml::Null) {
                self.warn(what.to_owned());
            }
        }
    }
}

/// The spec of a document, where its templates resolve, and its metadata. A Workflow that runs
/// a WorkflowTemplate joins its spec onto the template's.
pub(crate) struct Joined {
    pub spec: Node,
    pub path: PathBuf,
    pub metadata: Option<Node>,
}

pub(crate) fn joined_spec(document: &Document, library: &Library) -> Result<Joined> {
    let Some(spec_node) = document.spec() else {
        return Err(Failure::error(format!("{} has no spec", document.label())));
    };
    let mut spec_node = spec_node.clone();
    let mut context_path = document.path.clone();
    let mut metadata = document.node.get("metadata").cloned();
    if let Some(reference) = spec_node.get("workflowTemplateRef") {
        let name = reference.get("name").and_then(Node::as_str).unwrap_or("");
        let cluster = matches!(
            reference.get("clusterScope").map(|node| &node.value),
            Some(Yaml::Bool(true))
        );
        let kind = if cluster {
            Kind::ClusterWorkflowTemplate
        } else {
            Kind::WorkflowTemplate
        };
        let Some(template) = library.find(kind, name, &document.path) else {
            return Err(Failure::error(not_found(kind, name)));
        };
        let base = template.spec().cloned().unwrap_or_else(empty_mapping);
        remove_key(&mut spec_node, "workflowTemplateRef");
        spec_node = merge(&base, &spec_node);
        context_path = template.path.clone();
        if let Some(workflow_metadata) = base.get("workflowMetadata") {
            metadata = Some(match metadata {
                Some(own) => merge(workflow_metadata, &own),
                None => workflow_metadata.clone(),
            });
        }
    }
    Ok(Joined {
        spec: spec_node,
        path: context_path,
        metadata,
    })
}

/// The name of the workflow: its `metadata.name`, or its `generateName` with a suffix made from
/// the kind, name and position of the document, so that runs of the same document get the same
/// names wherever its file is.
pub(crate) fn workflow_name(document: &Document, metadata: Option<&Node>) -> String {
    let text = |key: &str| {
        document
            .node
            .get("metadata")
            .or(metadata)
            .and_then(|metadata| metadata.get(key))
            .and_then(Node::as_str)
            .unwrap_or("")
            .to_owned()
    };
    let seed = format!("{}#{}", document.label(), document.index);
    let suffix: String = format!("{:08x}", fnv32a(&seed))
        .chars()
        .map(|c| match c {
            '0'..='9' => c,
            _ => char::from(b'a' + (c as u8 - b'a') % 26),
        })
        .take(5)
        .collect();
    match document.kind {
        Kind::Workflow => match text("name") {
            name if name.is_empty() => format!("{}{suffix}", text("generateName")),
            name => name,
        },
        _ => {
            let name = document.name.trim_end_matches('-');
            format!("{name}-{suffix}")
        }
    }
}
