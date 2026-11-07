//! The ops of one workflow step: the op of its process, inside a branch for `when` and a map
//! for `scatter`, with the edges between them.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use cwl_parser::model::{RequirementClass, ScatterMethod, Step, short_name};
use wir::{ArrayType, Branch, Edge, Expr, Flow, Graph, Kind, Map, Op, Port, Scatter, Type};

use crate::workflow::end;
use crate::{Lowerer, Scope, blank};

/// The input of a branch that holds the condition of `when`.
const CONDITION: &str = "when";

/// The ops of a step, the last at the path of the step, and the edges between them.
pub(crate) struct Group {
    pub ops: Vec<Op>,
    pub edges: Vec<Edge>,
}

impl Group {
    /// The paths of the ops that take the step input `port` from outside the group.
    pub fn takers<'g>(&'g self, port: &'g str) -> impl Iterator<Item = &'g str> + 'g {
        (self.ops.iter())
            .filter(move |op| op.inputs.iter().any(|item| item.name == port))
            .filter(move |op| !self.edges.iter().any(|edge| fed(edge, &op.path, port)))
            .map(|op| op.path.as_str())
    }

    /// The ports that the group takes from outside, by name, with the type of the op of the
    /// step where it has one.
    fn inputs(&self) -> Vec<Port> {
        let mut ports: Vec<Port> = Vec::new();
        for op in self.ops.iter().rev() {
            for port in &op.inputs {
                let taken = self.takers(&port.name).any(|path| path == op.path);
                if taken && !ports.iter().any(|item| item.name == port.name) {
                    ports.push(port.clone());
                }
            }
        }
        ports
    }

    fn main(&self) -> &Op {
        &self.ops[self.ops.len() - 1]
    }
}

impl Lowerer {
    /// The group of a step: its op alone, or for `when` an expr of the condition and a branch
    /// that runs the op in a body graph of its own.
    pub(crate) fn guarded(
        &mut self,
        step: &Step,
        op: Op,
        path: &str,
        scope: &Scope,
    ) -> Result<Group, String> {
        let Some(when) = &step.when else {
            return Ok(Group {
                ops: vec![op],
                edges: Vec::new(),
            });
        };
        if op.inputs.iter().any(|port| port.name == CONDITION) {
            return Err(format!("step {path}: an input named {CONDITION}"));
        }
        let library = (scope.requirements.iter().rev())
            .find_map(|entry| match &entry.class {
                RequirementClass::InlineJavascript { expression_lib } => {
                    Some(expression_lib.clone())
                }
                _ => None,
            })
            .unwrap_or_default();
        let test = format!("{path}.{CONDITION}");
        let mut condition = blank(
            &test,
            Kind::Expr(Expr {
                language: "js".into(),
                code: when.text.clone(),
                library,
            }),
            op.origin.clone(),
        );
        condition.inputs = (step.inputs.iter())
            .filter(|input| !input.source.is_empty() || input.default.is_some())
            .map(|input| port(short_name(&input.id), Type::Dynamic))
            .collect();
        condition.outputs = vec![port("out", Type::Bool)];
        let body = self.graph_name(&format!("{path}.{CONDITION}"));
        let mut branch = blank(
            path,
            Kind::Branch(Branch {
                body: body.clone(),
                condition: CONDITION.into(),
            }),
            op.origin.clone(),
        );
        branch.inputs = op.inputs.clone();
        branch.inputs.push(port(CONDITION, Type::Bool));
        branch.outputs = (op.outputs.iter())
            .map(|item| port(&item.name, crate::types::optional(item.r#type.clone())))
            .collect();
        self.body(
            body,
            Group {
                ops: vec![op],
                edges: Vec::new(),
            },
            None,
        );
        Ok(Group {
            edges: vec![Edge {
                from: end(Some(&test), "out"),
                to: end(Some(path), CONDITION),
            }],
            ops: vec![condition, branch],
        })
    }

    /// A map op at `path` that runs the group of a scattered step once per round, in a body
    /// graph of its own.
    pub(crate) fn scatter(
        &mut self,
        step: &Step,
        group: Group,
        path: &str,
    ) -> Result<Group, String> {
        let over: Vec<String> = step
            .scatter
            .iter()
            .map(|id| short_name(id).to_owned())
            .collect();
        let inputs = group.inputs();
        if let Some(name) = over
            .iter()
            .find(|name| !inputs.iter().any(|port| port.name == **name))
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
        let mut map = blank(
            path,
            Kind::Map(Map {
                body: body.clone(),
                over: over.clone(),
                mode,
                parallelism: None,
                min_success: None,
            }),
            group.main().origin.clone(),
        );
        map.inputs = (inputs.iter())
            .map(|port| match over.contains(&port.name) {
                true => array(port.clone(), 1),
                false => port.clone(),
            })
            .collect();
        map.outputs = (group.main().outputs.iter())
            .map(|port| array(port.clone(), depth))
            .collect();
        self.body(body, group, Some(inputs));
        Ok(Group {
            ops: vec![map],
            edges: Vec::new(),
        })
    }

    /// Adds the graph `name` that runs a group, with the ports it takes and those of its op.
    fn body(&mut self, name: String, group: Group, inputs: Option<Vec<Port>>) {
        let inputs = inputs.unwrap_or_else(|| group.inputs());
        let main = group.main();
        let mut edges = group.edges.clone();
        for port in &inputs {
            for op in group.takers(&port.name) {
                edges.push(Edge {
                    from: end(None, &port.name),
                    to: end(Some(op), &port.name),
                });
            }
        }
        edges.extend(main.outputs.iter().map(|port| Edge {
            from: end(Some(&main.path), &port.name),
            to: end(None, &port.name),
        }));
        let outputs = main.outputs.clone();
        self.graphs.push(Graph {
            name,
            inputs,
            outputs,
            ops: group.ops,
            edges,
        });
    }

    /// A graph name from `stem` that no graph has yet.
    pub(crate) fn graph_name(&self, stem: &str) -> String {
        let mut name = stem.to_owned();
        while name == "main" || self.graphs.iter().any(|graph| graph.name == name) {
            name = format!("{stem}-{}", self.graphs.len() + 1);
        }
        name
    }
}

/// Whether an edge of the group feeds the input `port` of the op at `path`.
fn fed(edge: &Edge, path: &str, port: &str) -> bool {
    edge.to.op.as_deref() == Some(path) && edge.to.port == port
}

fn port(name: &str, r#type: Type) -> Port {
    Port {
        name: name.to_owned(),
        r#type,
        flow: Flow::Value,
        default: None,
    }
}

/// A port whose type is an array `depth` times, without the default of its items.
fn array(mut port: Port, depth: usize) -> Port {
    port.default = None;
    for _ in 0..depth {
        port.r#type = Type::Array(ArrayType {
            items: Box::new(port.r#type),
            nonempty: false,
        });
    }
    port
}
