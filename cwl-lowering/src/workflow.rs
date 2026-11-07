//! Lowers a CWL workflow into a wir graph: an op per step, an edge per plain link, a call op
//! with a graph of its own for each subworkflow, and a map op for each scattered step.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;

use cwl_parser::model::{Process, ScatterMethod, Step, Workflow, short_name};
use wir::{ArrayType, Call, Document, Edge, Endpoint, Graph, Kind, Map, Op, Port, Scatter, Type};

use crate::{Lowerer, Scope, blank, boundary, process_name};

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

    /// A map op at `path` that runs the op of a scattered step once per round, in a body
    /// graph of its own.
    fn scatter(&mut self, step: &Step, op: Op, path: &str) -> Result<Op, String> {
        let over: Vec<String> = step
            .scatter
            .iter()
            .map(|id| short_name(id).to_owned())
            .collect();
        if let Some(name) = over
            .iter()
            .find(|name| !op.inputs.iter().any(|port| port.name == **name))
        {
            return Err(format!("step {path}: the scattered input {name}"));
        }
        let mode = match step.scatter_method {
            None | Some(ScatterMethod::Dotproduct) => Scatter::Dot,
            Some(ScatterMethod::FlatCrossproduct) => Scatter::FlatCross,
            Some(ScatterMethod::NestedCrossproduct) => Scatter::NestedCross,
        };
        let depth = match mode {
            Scatter::NestedCross => over.len(),
            _ => 1,
        };
        let body = self.graph_name(path);
        let mut edges: Vec<Edge> = (op.inputs.iter())
            .map(|port| Edge {
                from: end(None, &port.name),
                to: end(Some(&op.path), &port.name),
            })
            .collect();
        edges.extend(op.outputs.iter().map(|port| Edge {
            from: end(Some(&op.path), &port.name),
            to: end(None, &port.name),
        }));
        let mut map = blank(
            path,
            Kind::Map(Map {
                body: body.clone(),
                over: over.clone(),
                mode,
                parallelism: None,
                min_success: None,
            }),
            op.origin.clone(),
        );
        map.inputs = (op.inputs.iter())
            .map(|port| match over.contains(&port.name) {
                true => array(port.clone(), 1),
                false => port.clone(),
            })
            .collect();
        map.outputs = op
            .outputs
            .iter()
            .map(|port| array(port.clone(), depth))
            .collect();
        self.graphs.push(Graph {
            name: body,
            inputs: op.inputs.clone(),
            outputs: op.outputs.clone(),
            ops: vec![op],
            edges,
        });
        Ok(map)
    }

    /// A graph name from `stem` that no graph has yet.
    fn graph_name(&self, stem: &str) -> String {
        let mut name = stem.to_owned();
        while name == "main" || self.graphs.iter().any(|graph| graph.name == name) {
            name = format!("{stem}-{}", self.graphs.len() + 1);
        }
        name
    }

    fn steps(
        &mut self,
        process: &Process,
        workflow: &Workflow,
        scope: &Scope,
        name: &str,
    ) -> Result<Graph, String> {
        if let Some(input) = process.inputs.iter().find(|input| input.default.is_some()) {
            return Err(format!("the default of input {}", short_name(&input.id)));
        }
        let (inputs, outputs) = boundary(process)?;
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
            let op = match step.scatter.is_empty() {
                true => op,
                false => self.scatter(step, op, path)?,
            };
            for input in &step.inputs {
                let port = short_name(&input.id);
                // A step input that the process does not take only serves expressions.
                if !op.inputs.iter().any(|item| item.name == port) {
                    continue;
                }
                if let [source] = input.source.as_slice() {
                    edges.push(Edge {
                        from: find(source)?,
                        to: end(Some(path), port),
                    });
                }
            }
            ops.push(op);
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
    if step.when.is_some() {
        return Err("when".into());
    }
    for input in &step.inputs {
        let port = short_name(&input.id);
        if input.value_from.is_some() || input.default.is_some() {
            return Err(format!("the valueFrom or default of input {port}"));
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

/// A port whose type is an array `depth` times.
fn array(mut port: Port, depth: usize) -> Port {
    for _ in 0..depth {
        port.r#type = Type::Array(ArrayType {
            items: Box::new(port.r#type),
            nonempty: false,
        });
    }
    port
}

fn end(op: Option<&str>, port: &str) -> Endpoint {
    Endpoint {
        op: op.map(str::to_owned),
        port: port.to_owned(),
    }
}
