//! The ops of one workflow step: the op of its process, after expr ops of `valueFrom`, inside
//! a branch for `when` and a map for `scatter`, with the edges between them.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use cwl_parser::model::{ScatterMethod, Step, short_name};
use wir::{ArrayType, Branch, Edge, Expr, Flow, Graph, Kind, Map, Op, Port, Scatter, Type};

use crate::tool::Computed;
use crate::workflow::end;
use crate::{Lowerer, Scope, blank};

/// The input of a branch that holds the condition of `when`.
const CONDITION: &str = "when";

/// The input of a `valueFrom` expr that holds the value of its step input.
pub(crate) const SELF: &str = "self";

/// The suffix of the task input that the `valueFrom` of a tool input fills.
pub(crate) const VALUE: &str = "valueFrom";

/// The ops of a step, the last at the path of the step, and the edges between them.
pub(crate) struct Group {
    pub ops: Vec<Op>,
    pub edges: Vec<Edge>,
    /// The `valueFrom` exprs by path, with the step input that their input `self` takes.
    pub selves: Vec<(String, String)>,
}

impl Group {
    /// A group of ops without edges between them.
    pub fn of(ops: Vec<Op>) -> Group {
        Group {
            ops,
            edges: Vec::new(),
            selves: Vec::new(),
        }
    }

    /// The ops and their inputs that take the step input `port` from outside the group.
    pub fn takers(&self, port: &str) -> Vec<(String, String)> {
        let mut found = Vec::new();
        for op in &self.ops {
            for input in &op.inputs {
                if self.outer(&op.path, &input.name) == Some(port) {
                    found.push((op.path.clone(), input.name.clone()));
                }
            }
        }
        found
    }

    /// The step input that an input of an op takes from outside the group, if it does.
    fn outer<'g>(&'g self, op: &str, input: &'g str) -> Option<&'g str> {
        if self.edges.iter().any(|edge| fed(edge, op, input)) {
            return None;
        }
        match self.selves.iter().find(|(path, _)| path == op) {
            Some((_, step)) if input == SELF => Some(step),
            _ => Some(input),
        }
    }

    /// The ports that the group takes from outside, by name, with the type of the op of the
    /// step where it has one.
    pub fn inputs(&self) -> Vec<Port> {
        let mut ports: Vec<Port> = Vec::new();
        for op in self.ops.iter().rev() {
            for port in &op.inputs {
                let Some(name) = self.outer(&op.path, &port.name) else {
                    continue;
                };
                if !ports.iter().any(|item| item.name == name) {
                    let mut taken = port.clone();
                    taken.name = name.to_owned();
                    ports.push(taken);
                }
            }
        }
        ports
    }

    pub fn main(&self) -> &Op {
        &self.ops[self.ops.len() - 1]
    }
}

impl Lowerer {
    /// The group of a step: an expr per `valueFrom` before its op, and for `when` an expr of
    /// the condition and a branch that runs the op in a body graph of its own.
    pub(crate) fn guarded(
        &mut self,
        step: &Step,
        inner: Group,
        path: &str,
        scope: &Scope,
    ) -> Result<Group, String> {
        let library = scope.library();
        let op = inner.main();
        let mut group = values(step, op, path, &library)?;
        let Some(when) = &step.when else {
            group.edges = feeds(&group, &inner);
            group.edges.extend(inner.edges);
            group.selves.extend(inner.selves);
            group.ops.extend(inner.ops);
            return Ok(group);
        };
        if inner.inputs().iter().any(|port| port.name == CONDITION) {
            return Err(format!("step {path}: an input named {CONDITION}"));
        }
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
            .filter(|input| {
                !input.source.is_empty() || input.default.is_some() || input.value_from.is_some()
            })
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
        branch.inputs = inner.inputs();
        branch.inputs.push(port(CONDITION, Type::Bool));
        branch.outputs = (op.outputs.iter())
            .map(|item| port(&item.name, crate::types::optional(item.r#type.clone())))
            .collect();
        self.body(body, inner, None);
        group.edges = feeds(&group, &Group::of(vec![condition.clone(), branch.clone()]));
        group.edges.push(Edge {
            from: end(Some(&test), "out"),
            to: end(Some(path), CONDITION),
        });
        group.ops.extend([condition, branch]);
        Ok(group)
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
        Ok(Group::of(vec![map]))
    }

    /// Adds the graph `name` that runs a group, with the ports it takes and those of its op.
    fn body(&mut self, name: String, group: Group, inputs: Option<Vec<Port>>) {
        let inputs = inputs.unwrap_or_else(|| group.inputs());
        let main = group.main();
        let mut edges = group.edges.clone();
        for port in &inputs {
            for (op, input) in group.takers(&port.name) {
                edges.push(Edge {
                    from: end(None, &port.name),
                    to: end(Some(&op), &input),
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

/// An expr op `<step>.<input>` for each `valueFrom` of the step: it sees the step inputs, as
/// they are before any `valueFrom`, and the value of its own input as `self`.
fn values(step: &Step, op: &Op, path: &str, library: &[String]) -> Result<Group, String> {
    let mut group = Group::of(Vec::new());
    for input in &step.inputs {
        let Some(code) = &input.value_from else {
            continue;
        };
        let name = short_name(&input.id);
        if [SELF, CONDITION].contains(&name) {
            return Err(format!(
                "step {path}: the valueFrom of an input named {name}"
            ));
        }
        let at = format!("{path}.{name}");
        let kind = Kind::Expr(Expr {
            language: "js".into(),
            code: code.text.clone(),
            library: library.to_vec(),
        });
        let mut expr = blank(&at, kind, op.origin.clone());
        expr.inputs = (step.inputs.iter())
            .map(|item| port(short_name(&item.id), Type::Dynamic))
            .chain([port(SELF, Type::Dynamic)])
            .collect();
        let taken = op.inputs.iter().find(|item| item.name == name);
        let r#type = taken.map_or(Type::Dynamic, |item| item.takes());
        expr.outputs = vec![port("out", r#type)];
        group.selves.push((at, name.to_owned()));
        group.ops.push(expr);
    }
    Ok(group)
}

/// The edges from the `valueFrom` exprs of a group to the ops of `inner` that take the step
/// input of each.
fn feeds(group: &Group, inner: &Group) -> Vec<Edge> {
    let mut edges = Vec::new();
    for (expr, name) in &group.selves {
        for (op, input) in inner.takers(name) {
            edges.push(Edge {
                from: end(Some(expr), "out"),
                to: end(Some(&op), &input),
            });
        }
    }
    edges
}

/// The group of a task whose inputs `values` fill: an expr `<task>.<port>` each, which sees
/// the task inputs and fills that task input with text, numbers, files or a list of them.
pub(crate) fn computed(mut task: Op, values: &[Computed], library: &[String]) -> Group {
    let mut group = Group::of(Vec::new());
    let file = Type::File(wir::FileType::default());
    let words = Type::Union(vec![
        Type::String,
        Type::Int,
        Type::Float,
        file,
        Type::Directory,
    ]);
    let words = crate::types::optional(Type::Union(vec![
        words.clone(),
        Type::Array(ArrayType {
            items: Box::new(words),
            nonempty: false,
        }),
    ]));
    for value in values {
        let at = format!("{}.{}", task.path, value.port);
        let kind = Kind::Expr(Expr {
            language: "js".into(),
            code: value.code.clone(),
            library: library.to_vec(),
        });
        let mut expr = blank(&at, kind, task.origin.clone());
        expr.inputs = task.inputs.clone();
        let this = value.this.as_ref();
        if let Some(found) =
            this.and_then(|name| task.inputs.iter().find(|item| item.name == *name))
        {
            expr.inputs.push(Port {
                name: SELF.into(),
                ..found.clone()
            });
        }
        let made = value.r#type.clone().unwrap_or_else(|| words.clone());
        expr.outputs = vec![port("out", made.clone())];
        group.edges.push(Edge {
            from: end(Some(&at), "out"),
            to: end(Some(&task.path), &value.port),
        });
        if let Some(name) = this {
            group.selves.push((at, name.clone()));
        }
        group.ops.push(expr);
    }
    let filled = (values.iter()).map(|value| {
        port(
            &value.port,
            value.r#type.clone().unwrap_or_else(|| words.clone()),
        )
    });
    task.inputs.extend(filled);
    group.ops.push(task);
    group
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
