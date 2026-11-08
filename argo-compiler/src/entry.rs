//! Compiles the entry graph of a workflow, which takes the workflow parameters and calls the
//! entrypoint, and the entry graph that runs the whole workflow as one native op.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use wir::{Graph, Kind, Language, Native, Type};

use crate::graphs::{Compiler, Place, Target, kept};
use crate::{ENTRY, Kept, SOURCE, artifact, edge, end, op, port, text, typed};

impl Compiler {
    /// The graphs of the workflow, the entry graph first: it takes the workflow parameters,
    /// calls the entrypoint and gives its outputs by their names.
    pub fn workflow(&mut self) -> Result<Vec<Graph>, Kept> {
        let spec = self.adapter.spec().clone();
        let at = |reason: &str| Err(kept(ENTRY, reason));
        if !spec.on_exit.is_empty() || !spec.hooks.is_empty() {
            return at("exit handlers and hooks of the workflow need their status as a value");
        }
        if spec.retry_strategy.is_some() {
            return at("a retryStrategy of the workflow retries steps and DAG templates too");
        }
        if spec.synchronization.is_some() || spec.suspend.is_some() {
            return at("synchronization and suspend hold the whole workflow");
        }
        if spec.parallelism.is_some() {
            return at("the parallelism of the workflow limits all its pods together");
        }
        if !spec.arguments.artifacts.is_empty() {
            return at("artifacts of the workflow arguments");
        }
        let entry = Place {
            name: self.adapter.entrypoint().to_owned(),
            reference: None,
        };
        let mut inputs = Vec::new();
        for parameter in &spec.arguments.parameters {
            let mut given = port(parameter.name.clone(), text());
            given.default = parameter.value.as_ref().map(|value| value.0.clone().into());
            inputs.push(given);
        }
        let mut graph = Graph {
            name: ENTRY.into(),
            inputs,
            outputs: Vec::new(),
            ops: Vec::new(),
            edges: Vec::new(),
        };
        let main = "main";
        let ports = match self.target(&entry, ENTRY)? {
            Target::Leaf(template) => crate::sites::entry_task(&mut graph, main, &entry, &template),
        };
        for output in ports {
            let name = (output.name.strip_prefix("outputs.parameters."))
                .or_else(|| output.name.strip_prefix("outputs.artifacts."));
            let Some(name) = name else { continue };
            if graph.outputs.iter().any(|known| known.name == name) {
                continue;
            }
            graph.outputs.push(port(name, output.r#type.clone()));
            graph
                .edges
                .push(edge(end(Some(main), &output.name), end(None, name)));
        }
        let mut graphs = vec![graph];
        graphs.append(&mut self.graphs);
        Ok(graphs)
    }

    /// The entry graph that runs the whole workflow as one native Argo op.
    pub fn native(&mut self) -> Result<Graph, String> {
        let spec = self.adapter.spec().clone();
        let entrypoint = self.adapter.entrypoint().to_owned();
        let template = (self.adapter.template(&entrypoint, None)).and_then(|node| typed(&node))?;
        let mut whole = op(
            ENTRY,
            Kind::Native(Native {
                source: SOURCE.into(),
                language: Language::Argo,
                entry: None,
                profiles: Vec::new(),
                configs: Vec::new(),
            }),
        );
        let mut graph = Graph {
            name: ENTRY.into(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            ops: Vec::new(),
            edges: Vec::new(),
        };
        for parameter in &spec.arguments.parameters {
            let mut given = port(parameter.name.clone(), text());
            given.default = parameter.value.as_ref().map(|value| value.0.clone().into());
            graph.inputs.push(given.clone());
            given.default = None;
            whole.inputs.push(given);
            let name = &parameter.name;
            graph
                .edges
                .push(edge(end(None, name), end(Some(ENTRY), name)));
        }
        let parameters = (template.outputs.parameters.iter()).map(|output| (&output.name, text()));
        let artifacts =
            (template.outputs.artifacts.iter()).map(|output| (&output.name, artifact()));
        for (name, r#type) in parameters.chain(artifacts) {
            if whole.outputs.iter().any(|known| &known.name == name) {
                continue;
            }
            let r#type = match r#type {
                Type::Optional(_) => r#type,
                other => Type::Optional(Box::new(other)),
            };
            whole.outputs.push(port(name.clone(), r#type.clone()));
            graph.outputs.push(port(name.clone(), r#type));
            graph
                .edges
                .push(edge(end(Some(ENTRY), name), end(None, name)));
        }
        graph.ops.push(whole);
        Ok(graph)
    }
}
