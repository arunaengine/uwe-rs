//! Lowers a CWL workflow into a wir graph: an op per step, an edge or merge and select ops per
//! link, a call op with a graph of its own for each subworkflow, and a map op per scatter.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;

use cwl_parser::model::{Process, Step, Workflow, short_name};
use wir::{Call, Document, Edge, Endpoint, Graph, Kind, Op, Type};

use crate::{Lowerer, Scope, blank, boundary, defaults, links, process_name, types};

impl Lowerer {
    /// The graph `name` of a workflow, or the first thing in it that has no lowering yet. A
    /// failed workflow leaves no graphs or kept ops of its steps behind.
    pub(crate) fn workflow(
        &mut self,
        process: &Process,
        workflow: &Workflow,
        scope: &Scope,
        name: &str,
    ) -> Result<Graph, String> {
        let mark = (self.kept.len(), self.graphs.len(), self.sources.len());
        let graph = self.steps(process, workflow, scope, name);
        if graph.is_err() {
            self.kept.truncate(mark.0);
            self.graphs.truncate(mark.1);
            self.sources.truncate(mark.2);
        }
        graph
    }

    /// A call op at `path` that runs the graph of a subworkflow.
    pub(crate) fn call(
        &mut self,
        process: &Process,
        workflow: &Workflow,
        scope: &Scope,
        path: &str,
    ) -> Result<Op, String> {
        let name = self.graph_name(&process_name(&process.id));
        let inner = format!("{}{path}/", self.prefix);
        let outer = std::mem::replace(&mut self.prefix, inner);
        let graph = self.workflow(process, workflow, scope, &name);
        self.prefix = outer;
        let graph = graph?;
        let mut op = blank(path, Kind::Call(Call { graph: name }), None);
        (op.inputs, op.outputs) = (graph.inputs.clone(), graph.outputs.clone());
        self.graphs.push(graph);
        Ok(op)
    }

    fn steps(
        &mut self,
        process: &Process,
        workflow: &Workflow,
        scope: &Scope,
        name: &str,
    ) -> Result<Graph, String> {
        // The steps would see the contents or listing that CWL loads into a workflow input.
        let loads = |input: &cwl_parser::model::InputParameter| {
            let binding = input
                .binding
                .as_ref()
                .and_then(|binding| binding.load_contents);
            input.load_contents == Some(true)
                || binding == Some(true)
                || input.load_listing.is_some()
        };
        if let Some(input) = process.inputs.iter().find(|input| loads(input)) {
            return Err(format!(
                "loadContents or loadListing of input {}",
                short_name(&input.id)
            ));
        }
        let (mut inputs, outputs) = boundary(process, scope)?;
        let named = types::named(scope);
        // A graph input keeps its type and takes null through its default.
        for (port, input) in inputs.iter_mut().zip(&process.inputs) {
            if let Some(default) = &input.default {
                let r#type = types::convert(&input.value_type, &named)?;
                let r#type = types::formatted(r#type, &input.format, &named)?;
                port.r#type = types::secondary(r#type, &input.secondary_files, true)?;
                defaults::set(port, default, &self.base)
                    .map_err(|why| format!("input {}: {why}", port.name))?;
            }
        }
        let mut ends: HashMap<&str, Endpoint> = HashMap::new();
        for input in &process.inputs {
            ends.insert(&input.id, end(None, short_name(&input.id)));
        }
        for step in &workflow.steps {
            for output in &step.outputs {
                ends.insert(output, end(Some(short_name(&step.id)), short_name(output)));
            }
        }
        let find = |source: &str| {
            (ends.get(source).cloned()).ok_or_else(|| format!("the source {}", short_name(source)))
        };
        let mut groups = Vec::new();
        for step in &workflow.steps {
            let path = short_name(&step.id);
            plain(step).map_err(|why| format!("step {path}: {why}"))?;
            let inner = scope.with(&step.requirements, &step.hints);
            let op = self.op(&step.run.process, &inner, path)?;
            let group = self.guarded(step, op, path, &inner)?;
            let mut group = match step.scatter.is_empty() {
                true => group,
                false => self.scatter(step, group, path)?,
            };
            for input in &step.inputs {
                let Some(default) = &input.default else {
                    continue;
                };
                let port = short_name(&input.id);
                for (op, name) in group.takers(port) {
                    let op = group.ops.iter_mut().find(|item| item.path == op);
                    let found =
                        op.and_then(|op| op.inputs.iter_mut().find(|item| item.name == name));
                    if let Some(item) = found {
                        defaults::set(item, default, &self.base)
                            .map_err(|why| format!("step {path}: input {port}: {why}"))?;
                    }
                }
            }
            groups.push((step, group));
        }
        // The type of what an end gives, to type the ops that merge and pick.
        let typed = |from: &Endpoint| {
            let ports = match &from.op {
                None => Some(&inputs),
                Some(path) => (groups.iter().flat_map(|(_, group)| &group.ops))
                    .find(|op| op.path == *path)
                    .map(|op| &op.outputs),
            };
            let port = ports.and_then(|ports| ports.iter().find(|port| port.name == from.port));
            port.map_or(Type::Dynamic, |port| port.r#type.clone())
        };
        let link = |at: &str, sources: &[String], input: (_, _)| {
            let ends = (sources.iter())
                .map(|source| find(source).map(|from| (from.clone(), typed(&from))))
                .collect::<Result<Vec<_>, String>>()?;
            links::join(at, &ends, input.0, input.1)
        };
        let (mut edges, mut joins) = (Vec::new(), Vec::new());
        for (step, group) in &groups {
            let path = short_name(&step.id);
            for input in step.inputs.iter().filter(|input| !input.source.is_empty()) {
                let port = short_name(&input.id);
                let joined = link(
                    &format!("{path}.{port}"),
                    &input.source,
                    (input.link_merge, input.pick_value),
                )
                .map_err(|why| format!("step {path}: input {port}: {why}"))?;
                // A step input that no op takes only serves expressions.
                for (op, input) in group.takers(port) {
                    edges.push(Edge {
                        from: joined.from.clone(),
                        to: end(Some(&op), &input),
                        read: Default::default(),
                    });
                }
                joins.extend(joined.ops);
                edges.extend(joined.edges);
            }
            edges.extend(group.edges.iter().cloned());
        }
        for output in &process.outputs {
            let port = short_name(&output.id);
            let joined = link(
                port,
                &output.output_source,
                (output.link_merge, output.pick_value),
            )
            .map_err(|why| format!("output {port}: {why}"))?;
            edges.push(Edge {
                from: joined.from,
                to: end(None, port),
                read: Default::default(),
            });
            joins.extend(joined.ops);
            edges.extend(joined.edges);
        }
        let mut ops: Vec<Op> = groups
            .into_iter()
            .flat_map(|(_, group)| group.ops)
            .collect();
        ops.extend(joins);
        let graph = Graph {
            name: name.to_owned(),
            inputs,
            outputs,
            ops,
            edges,
        };
        self.check(&graph)?;
        Ok(graph)
    }

    /// The first problem that the verifier finds in a graph, with the graphs it calls.
    pub(crate) fn check(&self, graph: &Graph) -> Result<(), String> {
        let mut graphs = self.graphs.clone();
        graphs.push(graph.clone());
        let document = Document {
            wir_version: wir::VERSION.into(),
            sources: self.sources.clone(),
            types: Vec::new(),
            graphs,
            entry: graph.name.clone(),
            hooks: Vec::new(),
        };
        match wir::verify(&document) {
            Ok(()) => Ok(()),
            Err(problems) => Err(problems[0].to_string()),
        }
    }
}

/// Refuses step features that have no lowering yet.
fn plain(step: &Step) -> Result<(), String> {
    for input in &step.inputs {
        let port = short_name(&input.id);
        if input.load_contents.is_some() || input.load_listing.is_some() {
            return Err(format!("loadContents or loadListing of input {port}"));
        }
    }
    Ok(())
}

pub(crate) fn end(op: Option<&str>, port: &str) -> Endpoint {
    Endpoint {
        op: op.map(str::to_owned),
        port: port.to_owned(),
    }
}
