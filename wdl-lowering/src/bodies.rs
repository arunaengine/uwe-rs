//! The body graphs of scatters and conditionals, as map and branch ops, and the merge and
//! select ops where the values of conditionals meet.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use wdl_analysis::Document as Analyzed;
use wdl_ast::AstToken;
use wdl_ast::v1::{ConditionalStatement, Expr, LiteralExpr, ScatterStatement, WorkflowStatement};
use wir::reserved::{Merge, Pick, Select};
use wir::{Branch, Graph, Kind, Map, Port, Scatter, Type};

use crate::types::{array, convert, optional, port};
use crate::workflow::{Bound, Frame, Held, Shape, direct, place, ports, wire};
use crate::{Lowerer, blank, edge, end};

/// The input of a branch op that holds the condition of an `if`.
const CONDITION: &str = "if.condition";

impl Lowerer {
    /// A map op at `path` that runs the body of a scatter once per item, in a graph of its own.
    pub(crate) fn scatter(
        &mut self,
        document: &Analyzed,
        scatter: &ScatterStatement,
        path: &str,
        frame: &mut Frame,
    ) -> Result<(), String> {
        let variable = scatter.variable().text().to_owned();
        let body: Vec<WorkflowStatement> = scatter.statements().collect();
        let items = match direct(frame, &scatter.expr()).map(|held| held.r#type) {
            Some(Type::Array(array)) => *array.items,
            _ => item_type(document, &body, &variable),
        };
        let visible = frame.visible();
        let mut inner = Frame::new(&visible, frame.origin.clone());
        inner.nested = frame.nested.is_some().then(Vec::new);
        inner.body = true;
        let held = Held {
            end: end(None, &variable),
            r#type: items.clone(),
        };
        inner.local.insert(variable.clone(), Bound::Value(held));
        self.nested(document, &body, &mut inner, path)?;
        let nested = names(&inner);
        let variable_port = port(&variable, items.clone());
        let (graph, outputs) = self.body(document, &body, inner, path, Some(variable_port))?;
        let kind = Kind::Map(Map {
            body: graph.name.clone(),
            over: vec![variable.clone()],
            mode: Scatter::Dot,
            parallelism: None,
            min_success: None,
        });
        let mut map = blank(path, kind, frame.origin.clone());
        // A scatter adds no name: its tasks give their shard after the name of their call.
        map.attributes.name = Some(String::new());
        map.inputs = graph.inputs.clone();
        map.inputs[0].r#type = array(items.clone());
        map.outputs = (outputs.iter())
            .map(|item| port(&item.name, array(item.r#type.clone())))
            .collect();
        let at = format!("{path}.items");
        let source = wire(frame, &scatter.expr(), &array(items), &at)?;
        frame
            .edges
            .push(edge(source.end, end(Some(path), &variable)));
        takes(frame, &graph, path, Some(&variable), &nested);
        self.graphs.push(graph);
        frame.ops.push(map);
        Ok(())
    }

    /// A branch op at `path` that runs the body of an `if` when its condition holds.
    pub(crate) fn conditional(
        &mut self,
        document: &Analyzed,
        conditional: &ConditionalStatement,
        path: &str,
        frame: &mut Frame,
    ) -> Result<(), String> {
        let clause = conditional.if_clause();
        let body: Vec<WorkflowStatement> = clause.statements().collect();
        let visible = frame.visible();
        let mut inner = Frame::new(&visible, frame.origin.clone());
        inner.nested = frame.nested.is_some().then(Vec::new);
        inner.body = true;
        self.nested(document, &body, &mut inner, path)?;
        let nested = names(&inner);
        let (graph, outputs) = self.body(document, &body, inner, path, None)?;
        let kind = Kind::Branch(Branch {
            body: graph.name.clone(),
            condition: CONDITION.into(),
        });
        let mut branch = blank(path, kind, frame.origin.clone());
        branch.attributes.name = Some(String::new());
        branch.inputs = graph.inputs.clone();
        branch.inputs.push(port(CONDITION, Type::Bool));
        branch.outputs = (outputs.iter())
            .map(|item| port(&item.name, optional(item.r#type.clone())))
            .collect();
        let expr = clause.expr().ok_or("an if without a condition")?;
        let condition = wire(frame, &expr, &Type::Bool, &format!("{path}.condition"))?;
        frame
            .edges
            .push(edge(condition.end, end(Some(path), CONDITION)));
        takes(frame, &graph, path, None, &nested);
        self.graphs.push(graph);
        frame.ops.push(branch);
        Ok(())
    }

    /// The ops of a body, whose kept ops get paths below the op of its scatter or `if`.
    fn nested(
        &mut self,
        document: &Analyzed,
        body: &[WorkflowStatement],
        inner: &mut Frame,
        path: &str,
    ) -> Result<(), String> {
        let prefix = format!("{}{path}/", self.prefix);
        let outer = std::mem::replace(&mut self.prefix, prefix);
        let done = self.block(document, body, inner);
        self.prefix = outer;
        done
    }

    /// The body graph of a scatter or an `if`: the names it takes are its inputs, after the
    /// variable of a scatter, and the names its statements define are its outputs, with the
    /// types they have in the body.
    fn body(
        &mut self,
        document: &Analyzed,
        statements: &[WorkflowStatement],
        mut inner: Frame,
        path: &str,
        variable: Option<Port>,
    ) -> Result<(Graph, Vec<Port>), String> {
        // wir has no `T??` that is `T?`, so an optional output of an `if` body is dynamic.
        let open = variable.is_none();
        let mut outputs = Vec::new();
        let mut graph = Graph {
            name: self.graph_name(path),
            inputs: variable.into_iter().collect(),
            outputs: Vec::new(),
            ops: std::mem::take(&mut inner.ops),
            edges: std::mem::take(&mut inner.edges),
        };
        for (name, bound) in &inner.taken {
            for (item, held) in ports(name, bound) {
                graph.inputs.push(port(&item, held.r#type));
            }
        }
        graph.inputs.extend(inner.nested.take().unwrap_or_default());
        for statement in statements {
            for (name, _) in place(document, statement)?.1 {
                let Some(bound) = inner.local.get(&name) else {
                    continue;
                };
                for (item, held) in ports(&name, bound) {
                    let r#type = match (open, &held.r#type) {
                        (true, Type::Optional(_)) => Type::Dynamic,
                        (_, r#type) => r#type.clone(),
                    };
                    graph.outputs.push(port(&item, r#type));
                    outputs.push(port(&item, held.r#type.clone()));
                    graph.edges.push(edge(held.end, end(None, &item)));
                }
            }
        }
        self.check(&graph)?;
        Ok((graph, outputs))
    }
}

/// The nested call inputs of a body, which the graph around it takes as nested inputs too.
fn names(inner: &Frame) -> Vec<String> {
    let nested = inner.nested.iter().flatten();
    nested.map(|item| item.name.clone()).collect()
}

/// The edges into a map or branch op from the names its body graph takes, and from the
/// nested call inputs of its body.
fn takes(frame: &mut Frame, graph: &Graph, path: &str, variable: Option<&str>, nested: &[String]) {
    for item in graph
        .inputs
        .iter()
        .filter(|item| nested.contains(&item.name))
    {
        if let Some(outer) = &mut frame.nested {
            outer.push(item.clone());
            let to = end(Some(path), &item.name);
            frame.edges.push(edge(end(None, &item.name), to));
        }
    }
    for item in graph
        .inputs
        .iter()
        .filter(|item| Some(item.name.as_str()) != variable && !nested.contains(&item.name))
    {
        let (name, member) = match item.name.split_once('.') {
            Some((name, member)) => (name, Some(member)),
            None => (item.name.as_str(), None),
        };
        let found = match (frame.get(name), member) {
            (Some(Bound::Value(held)), None) => Some(held),
            (Some(Bound::Call(outputs)), Some(member)) => (outputs.into_iter())
                .find(|(output, _)| output == member)
                .map(|(_, held)| held),
            _ => None,
        };
        if let Some(held) = found {
            frame
                .edges
                .push(edge(held.end, end(Some(path), &item.name)));
        }
    }
}

/// The names that the statements of a body define, with their types wrapped for the graph
/// around it.
pub(crate) fn inner(
    document: &Analyzed,
    body: &[WorkflowStatement],
    wrap: &dyn Fn(Type) -> Type,
) -> Result<Vec<(String, Shape)>, String> {
    let mut defined = Vec::new();
    for statement in body {
        for (name, shape) in place(document, statement)?.1 {
            let shape = match shape {
                Shape::Value(r#type) => Shape::Value(wrap(r#type)),
                Shape::Call(outputs) => Shape::Call(
                    (outputs.into_iter())
                        .map(|(output, r#type)| (output, wrap(r#type)))
                        .collect(),
                ),
            };
            defined.push((name, shape));
        }
    }
    Ok(defined)
}

/// The type of the variable of a scatter, as the analysis of the document found it.
fn item_type(document: &Analyzed, body: &[WorkflowStatement], variable: &str) -> Type {
    let found = body.first().and_then(|statement| {
        let start = usize::from(statement.inner().text_range().start());
        let scope = document.find_scope_by_position(start)?;
        convert(scope.lookup(variable)?.ty()).ok()
    });
    found.unwrap_or(Type::Dynamic)
}

/// A merge op and a select op at `path` for `select_first` or `select_all` of an array of
/// plain references, where the values of several branches meet.
pub(crate) fn selected(frame: &mut Frame, expr: &Expr, path: &str, output: &Type) -> bool {
    let Expr::Call(call) = expr else {
        return false;
    };
    let (rule, item) = match (call.target().text(), output) {
        ("select_first", _) => (Pick::First, output.clone()),
        ("select_all", Type::Array(array)) => (Pick::All, (*array.items).clone()),
        _ => return false,
    };
    let arguments: Vec<Expr> = call.arguments().collect();
    let [Expr::Literal(LiteralExpr::Array(items))] = arguments.as_slice() else {
        return false;
    };
    let want = optional(item);
    let mut held = Vec::new();
    for item in items.elements() {
        match direct(frame, &item) {
            Some(found) if want.accepts(&found.r#type, &[]) => held.push(found),
            _ => return false,
        }
    }
    if held.is_empty() {
        return false;
    }
    let merged = format!("{path}.items");
    let mut merge = blank(&merged, Kind::Merge(Merge::default()), frame.origin.clone());
    for (index, found) in held.into_iter().enumerate() {
        let name = index.to_string();
        merge.inputs.push(port(&name, found.r#type));
        frame.edges.push(edge(found.end, end(Some(&merged), &name)));
    }
    merge.outputs = vec![port("value", array(want.clone()))];
    let mut select = blank(path, Kind::Select(Select { rule }), frame.origin.clone());
    select.inputs = vec![port("items", array(want))];
    select.outputs = vec![port("value", output.clone())];
    frame
        .edges
        .push(edge(end(Some(&merged), "value"), end(Some(path), "items")));
    frame.ops.extend([merge, select]);
    true
}
