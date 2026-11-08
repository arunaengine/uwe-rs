//! Compiles the workflow and its steps and DAG templates into graphs, one per template.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use argo_parser::model::Template;
use argo_runtime::adapter::Adapter;
use wir::Graph;

use crate::{Kept, typed};

/// Where a template is: its name, and the WorkflowTemplate that holds it with whether that
/// is cluster scoped.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Place {
    pub name: String,
    pub reference: Option<(String, bool)>,
}

/// What a call reaches: a leaf template that a dialect task runs.
pub(crate) enum Target {
    Leaf(Box<Template>),
}

pub(crate) struct Compiler {
    pub adapter: Adapter,
    pub graphs: Vec<Graph>,
}

impl Compiler {
    pub fn new(adapter: Adapter) -> Self {
        Compiler {
            adapter,
            graphs: Vec::new(),
        }
    }

    /// What a call of the template at `place` reaches.
    pub fn target(&mut self, place: &Place, at: &str) -> Result<Target, Kept> {
        let node = (self.adapter.template(&place.name, place.reference.as_ref()))
            .map_err(|reason| kept(at, &reason))?;
        let template = typed(&node).map_err(|reason| kept(at, &reason))?;
        if template.steps.is_empty() && node.get("steps").is_none() && template.dag.is_none() {
            if let Some(reason) = leaf_reason(&template) {
                return Err(kept(at, &format!("template {}: {reason}", template.name)));
            }
            if let Some(name) = unsafe_name(&template) {
                return Err(kept(at, &format!("the name {name} holds a tag")));
            }
            return Ok(Target::Leaf(Box::new(template)));
        }
        let reason = "steps and DAG templates do not compile into graphs yet";
        Err(kept(at, &format!("template {}: {reason}", template.name)))
    }
}

/// A name of an input or output of a template that is no wir name, like one with a tag.
fn unsafe_name(template: &Template) -> Option<&str> {
    let inputs = &template.inputs;
    let outputs = &template.outputs;
    let parameters = (inputs.parameters.iter())
        .chain(&outputs.parameters)
        .map(|p| p.name.as_str());
    let artifacts = (inputs.artifacts.iter())
        .chain(&outputs.artifacts)
        .map(|a| a.name.as_str());
    parameters
        .chain(artifacts)
        .find(|name| crate::safe(name) != *name || name.is_empty())
}

/// Why a leaf template stays native, if it does.
fn leaf_reason(template: &Template) -> Option<&'static str> {
    if template.suspend.is_some() {
        return Some("a suspend template waits for a person or a time");
    }
    if template.resource.is_some() {
        return Some("a resource template changes Kubernetes objects");
    }
    if template.daemon == Some(true) {
        return Some("a daemon serves other steps until its template ends");
    }
    if !template.sidecars.is_empty() {
        return Some("sidecars run beside the main container until it ends");
    }
    if template.synchronization.is_some() {
        return Some("synchronization holds locks across the workflow");
    }
    if template.plugin.is_some() || template.data.is_some() {
        return Some("plugin and data templates");
    }
    let runs = template.container.is_some()
        || template.script.is_some()
        || template.container_set.is_some()
        || template.http.is_some();
    (!runs).then_some("a template without a type")
}

pub(crate) fn kept(path: &str, reason: &str) -> Kept {
    Kept {
        path: path.to_owned(),
        reason: reason.to_owned(),
    }
}
