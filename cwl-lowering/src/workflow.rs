//! Lowers a CWL workflow into a wir graph: an op per step, an edge per plain link, a call op
//! with a graph of its own for each subworkflow, and a map op for each scattered step.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;

use cwl_parser::model::{Process, Step, Workflow, short_name};
use wir::{Call, Document, Edge, Endpoint, Graph, Kind, Op};

use crate::{Lowerer, Scope, blank, boundary, defaults, process_name, types};

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
        let (mut inputs, outputs) = boundary(process)?;
        // A graph input keeps its type and takes null through its default.
        for (port, input) in inputs.iter_mut().zip(&process.inputs) {
            if let Some(default) = &input.default {
                port.r#type = types::convert(&input.value_type)?;
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
        let mut ops = Vec::new();
        let mut edges = Vec::new();
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
                let takers: Vec<String> = group.takers(port).map(str::to_owned).collect();
                for op in group.ops.iter_mut().filter(|op| takers.contains(&op.path)) {
                    let found = op.inputs.iter_mut().find(|item| item.name == port);
                    if let Some(item) = found {
                        defaults::set(item, default, &self.base)
                            .map_err(|why| format!("step {path}: input {port}: {why}"))?;
                    }
                }
            }
            for input in &step.inputs {
                let port = short_name(&input.id);
                let [source] = input.source.as_slice() else {
                    continue;
                };
                // A step input that no op takes only serves expressions.
                for op in group.takers(port) {
                    edges.push(Edge {
                        from: find(source)?,
                        to: end(Some(op), port),
                    });
                }
            }
            ops.extend(group.ops);
            edges.extend(group.edges);
        }
        for output in &process.outputs {
            let port = short_name(&output.id);
            let ([source], None, None) = (
                output.output_source.as_slice(),
                &output.link_merge,
                &output.pick_value,
            ) else {
                return Err(format!("output {port}: not one plain source"));
            };
            edges.push(Edge {
                from: find(source)?,
                to: end(None, port),
            });
        }
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
    fn check(&self, graph: &Graph) -> Result<(), String> {
        let mut graphs = self.graphs.clone();
        graphs.push(graph.clone());
        let document = Document {
            wir_version: wir::VERSION.into(),
            sources: self.sources.clone(),
            types: Vec::new(),
            graphs,
            entry: graph.name.clone(),
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
        if input.value_from.is_some() {
            return Err(format!("the valueFrom of input {port}"));
        }
        if input.source.len() > 1 || input.link_merge.is_some() || input.pick_value.is_some() {
            return Err(format!("several sources for input {port}"));
        }
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
