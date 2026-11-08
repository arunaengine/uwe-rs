//! Lowers a WDL workflow into a wir graph: an expr op per declaration, a call or native op per
//! call, a map op per scatter and a branch op per conditional, each with a body graph.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, BTreeSet};

use wdl_analysis::Document as Analyzed;
use wdl_analysis::types::Optional;
use wdl_ast::AstToken;
use wdl_ast::v1::{CallStatement, Decl, Expr, WorkflowStatement};
use wdl_runtime::Value;
use wdl_runtime::document::{self, Callable, Unit, declared};
use wdl_runtime::expression::names;
use wir::{Dependency, Edge, Endpoint, Graph, Kind, Op, Origin, Port, Status, Type};

use crate::bodies::{inner, selected};
use crate::types::{array, convert, optional, port};
use crate::{Lowerer, blank, boundary, constant, edge, end, origin, reads, text};

/// A value where an op can read it, with its type.
#[derive(Clone, Debug)]
pub(crate) struct Held {
    pub end: Endpoint,
    pub r#type: Type,
}

/// What a WDL name stands for: a value, or the outputs of a call by name.
#[derive(Clone, Debug)]
pub(crate) enum Bound {
    Value(Held),
    Call(Vec<(String, Held)>),
}

/// What a statement defines, with types as the graph around it sees them.
#[derive(Clone, Debug)]
pub(crate) enum Shape {
    Value(Type),
    Call(Vec<(String, Type)>),
}

/// The ops of one graph and the names they read: its own, and those it takes from the graph
/// around it as inputs.
pub(crate) struct Frame<'o> {
    pub local: BTreeMap<String, Bound>,
    pub outer: &'o BTreeMap<String, Bound>,
    /// The outer names this graph takes, as the graph around it binds them.
    pub taken: BTreeMap<String, Bound>,
    /// The op of this graph that runs each call, also inside a scatter or an `if`.
    pub owners: BTreeMap<String, String>,
    pub ops: Vec<Op>,
    pub edges: Vec<Edge>,
    pub origin: Option<Origin>,
    /// The inputs of the calls of the entry workflow that the calls do not give, as inputs of
    /// the entry graph `<call>.<input>`, as WDL input files may give them.
    pub nested: Option<Vec<Port>>,
}

impl<'o> Frame<'o> {
    pub(crate) fn new(outer: &'o BTreeMap<String, Bound>, origin: Option<Origin>) -> Self {
        Frame {
            local: BTreeMap::new(),
            outer,
            taken: BTreeMap::new(),
            owners: BTreeMap::new(),
            ops: Vec::new(),
            edges: Vec::new(),
            origin,
            nested: None,
        }
    }

    /// What a name stands for here; an outer name becomes an input of this graph.
    pub(crate) fn get(&mut self, name: &str) -> Option<Bound> {
        if let Some(bound) = self.local.get(name) {
            return Some(bound.clone());
        }
        let outer = self.outer.get(name)?.clone();
        self.taken.insert(name.to_owned(), outer.clone());
        let inside = |port: &str, held: &Held| Held {
            end: end(None, port),
            r#type: held.r#type.clone(),
        };
        Some(match &outer {
            Bound::Value(held) => Bound::Value(inside(name, held)),
            Bound::Call(outputs) => Bound::Call(
                (outputs.iter())
                    .map(|(output, held)| {
                        let port = format!("{name}.{output}");
                        (output.clone(), inside(&port, held))
                    })
                    .collect(),
            ),
        })
    }

    /// Every name that a body graph inside this one can read.
    pub(crate) fn visible(&self) -> BTreeMap<String, Bound> {
        let mut names = self.outer.clone();
        names.extend(self.local.clone());
        names
    }
}

/// The ports that hold a name: the name for a value, `<call>.<output>` for a call.
pub(crate) fn ports(name: &str, bound: &Bound) -> Vec<(String, Held)> {
    match bound {
        Bound::Value(held) => vec![(name.to_owned(), held.clone())],
        Bound::Call(outputs) => (outputs.iter())
            .map(|(output, held)| (format!("{name}.{output}"), held.clone()))
            .collect(),
    }
}

impl Lowerer {
    /// The graph `name` of a workflow, or the first thing in it that has no lowering yet. A
    /// failed workflow leaves no graphs or kept ops of its own behind.
    pub(crate) fn workflow(&mut self, unit: &Unit, name: &str) -> Result<Graph, String> {
        let mark = (self.kept.len(), self.graphs.len(), self.done.len());
        let held = self.fallbacks.len();
        let graph = self.statements(unit, name);
        if graph.is_err() {
            self.kept.truncate(mark.0);
            self.graphs.truncate(mark.1);
            self.done.truncate(mark.2);
            self.fallbacks.truncate(held);
        }
        graph
    }

    /// The op at `path` that runs a call of a workflow: a call of its graph, else a native op.
    pub(crate) fn subworkflow(&mut self, unit: &Unit, path: &str) -> Result<Op, String> {
        let graph = match self.known(unit) {
            Some(graph) => graph,
            None => {
                let inner = format!("{}{path}/", self.prefix);
                let outer = std::mem::replace(&mut self.prefix, inner);
                let graph = self.workflow(unit, &unit.name());
                self.prefix = outer;
                let graph = graph.map(|mut graph| {
                    graph.name = self.graph_name(&unit.name());
                    let renamed = self
                        .fallbacks
                        .iter_mut()
                        .filter(|item| item.0 == unit.name());
                    renamed.for_each(|item| item.0 = graph.name.clone());
                    let name = graph.name.clone();
                    self.graphs.push(graph);
                    name
                });
                self.remember(unit, graph.clone());
                graph
            }
        };
        let source = self.source(&unit.document);
        match graph {
            Ok(name) => Ok(self.call(&name, path, origin(&source, &unit.name()))),
            Err(reason) => self.native(unit, path, reason),
        }
    }

    fn statements(&mut self, unit: &Unit, name: &str) -> Result<Graph, String> {
        let Callable::Workflow(workflow) = &unit.callable else {
            return Err("not a workflow".into());
        };
        let document = &unit.document;
        let source = self.source(document);
        let empty = BTreeMap::new();
        let mut frame = Frame::new(&empty, origin(&source, &unit.name()));
        let entry = name == "main";
        frame.nested = entry.then(Vec::new);
        let mut inputs = Vec::new();
        let mut defaults = Vec::new();
        for decl in workflow
            .input()
            .iter()
            .flat_map(|section| section.declarations())
        {
            let input = decl.name().text().to_owned();
            let ty = declared(document, &decl.ty())?;
            let r#type = convert(&ty).map_err(|why| format!("input {input}: {why}"))?;
            let mut item = port(&input, r#type.clone());
            let mut held = Held {
                end: end(None, &input),
                r#type: r#type.clone(),
            };
            if let Decl::Bound(bound) = &decl {
                let expr = bound.expr();
                match constant(&expr).map(|value| value.coerce(&ty)) {
                    Some(Ok(Value::None)) => item.r#type = optional(r#type),
                    Some(Ok(value)) if ty.is_optional() && !entry => {
                        let fallback = (name.to_owned(), input.clone(), value.result_json());
                        self.fallbacks.push(fallback);
                    }
                    Some(Ok(value)) => item.default = Some(value.result_json()),
                    _ => {
                        item.r#type = optional(r#type.clone());
                        held.end = end(Some(&input), "value");
                        defaults.push((input.clone(), expr, r#type));
                    }
                }
            }
            frame.local.insert(input, Bound::Value(held));
            inputs.push(item);
        }
        let statements: Vec<WorkflowStatement> = workflow.statements().collect();
        self.block(document, &statements, &mut frame)?;
        // A default that is not a literal is an expr op that takes the given value first.
        for (input, expr, r#type) in defaults {
            let code = format!("if defined({input}) then {input} else ({})", text(&expr));
            let mut used = names(&expr);
            used.remove(&input);
            evaluated(&mut frame, &input, code, &expr, used, r#type.clone())?;
            if let Some(op) = frame.ops.last_mut() {
                op.inputs.push(port(&input, optional(r#type)));
            }
            frame
                .edges
                .push(edge(end(None, &input), end(Some(&input), &input)));
        }
        let mut outputs = Vec::new();
        for decl in workflow
            .output()
            .iter()
            .flat_map(|section| section.declarations())
        {
            let output = decl.name().text().to_owned();
            let ty = declared(document, &decl.ty())?;
            let r#type = convert(&ty).map_err(|why| format!("output {output}: {why}"))?;
            let at = format!("output.{output}");
            let held = wire(&mut frame, &decl.expr(), &r#type, &at)?;
            frame.edges.push(edge(held.end.clone(), end(None, &output)));
            frame.local.insert(output.clone(), Bound::Value(held));
            outputs.push(port(&output, r#type));
        }
        inputs.extend(frame.nested.take().unwrap_or_default());
        let graph = Graph {
            name: name.to_owned(),
            inputs,
            outputs,
            ops: frame.ops,
            edges: frame.edges,
        };
        self.check(&graph)?;
        Ok(graph)
    }

    /// The ops of the statements of one graph, which may refer to each other in any order.
    pub(crate) fn block(
        &mut self,
        document: &Analyzed,
        statements: &[WorkflowStatement],
        frame: &mut Frame,
    ) -> Result<(), String> {
        let mut paths: Vec<String> = Vec::new();
        for statement in statements {
            let (path, defined) = place(document, statement)?;
            let path = match paths.contains(&path) {
                true => format!("{path}-{}", paths.len() + 1),
                false => path,
            };
            let direct = matches!(
                statement,
                WorkflowStatement::Declaration(_) | WorkflowStatement::Call(_)
            );
            for (name, shape) in defined {
                let bound = match shape {
                    Shape::Value(r#type) => {
                        let port = if direct { "value" } else { &name };
                        let end = end(Some(&path), port);
                        Bound::Value(Held { end, r#type })
                    }
                    Shape::Call(outputs) => {
                        frame.owners.insert(name.clone(), path.clone());
                        let held = |output: &str, r#type: Type| Held {
                            end: match direct {
                                true => end(Some(&path), output),
                                false => end(Some(&path), &format!("{name}.{output}")),
                            },
                            r#type,
                        };
                        Bound::Call(
                            (outputs.into_iter())
                                .map(|(output, r#type)| (output.clone(), held(&output, r#type)))
                                .collect(),
                        )
                    }
                };
                frame.local.insert(name, bound);
            }
            paths.push(path);
        }
        for (statement, path) in statements.iter().zip(&paths) {
            match statement {
                WorkflowStatement::Declaration(decl) => {
                    let name = decl.name().text().to_owned();
                    let ty = declared(document, &decl.ty())?;
                    let r#type = convert(&ty).map_err(|why| format!("{name}: {why}"))?;
                    let expr = decl.expr();
                    if !selected(frame, &expr, path, &r#type) {
                        evaluated(frame, path, text(&expr), &expr, names(&expr), r#type)?;
                    }
                }
                WorkflowStatement::Call(call) => self.called(document, call, frame)?,
                WorkflowStatement::Scatter(scatter) => {
                    self.scatter(document, scatter, path, frame)?
                }
                WorkflowStatement::Conditional(conditional) => {
                    self.conditional(document, conditional, path, frame)?
                }
            }
        }
        Ok(())
    }

    /// The call or native op of a call, with an edge or an expr op per input.
    fn called(
        &mut self,
        document: &Analyzed,
        call: &CallStatement,
        frame: &mut Frame,
    ) -> Result<(), String> {
        let unit = document::called(document, &call.target())?;
        let alias = alias(call, &unit);
        let mut op = match &unit.callable {
            Callable::Task(_) => self.task(&unit, &alias)?,
            Callable::Workflow(_) => self.subworkflow(&unit, &alias)?,
        };
        let mut given = BTreeSet::new();
        for input in call.inputs() {
            let name = input.name().text().to_owned();
            given.insert(name.clone());
            let found = op.inputs.iter().find(|port| port.name == name);
            let want = found.ok_or_else(|| format!("call {alias}: no input {name}"))?;
            let want = match want.default {
                Some(_) => optional(want.r#type.clone()),
                None => want.r#type.clone(),
            };
            let held = match input.expr() {
                Some(expr) => wire(frame, &expr, &want, &format!("{alias}.{name}"))?,
                None => match frame.get(&name) {
                    Some(Bound::Value(held)) if want.accepts(&held.r#type, &[]) => held,
                    _ => return Err(format!("call {alias}: the input {name}")),
                },
            };
            frame.edges.push(edge(held.end, end(Some(&alias), &name)));
            // A given value, also `None`, takes the place of a default.
            if let Some(port) = op.inputs.iter_mut().find(|port| port.name == name) {
                port.default = None;
            }
        }
        if let Some(nested) = &mut frame.nested {
            for item in op.inputs.iter().filter(|item| !given.contains(&item.name)) {
                let outer = format!("{alias}.{}", item.name);
                let r#type = match item.default.is_some() {
                    true => optional(item.r#type.clone()),
                    false => item.r#type.clone(),
                };
                nested.push(port(&outer, r#type));
                frame
                    .edges
                    .push(edge(end(None, &outer), end(Some(&alias), &item.name)));
            }
        }
        for after in call.after() {
            let name = after.name().text().to_owned();
            let owner = frame.owners.get(&name).cloned();
            let owner = owner.ok_or_else(|| format!("call {alias}: after {name} outside"))?;
            op.after.push(Dependency {
                op: owner,
                on: vec![Status::Succeeded, Status::Skipped],
            });
        }
        frame.ops.push(op);
        Ok(())
    }

    /// The first problem that the verifier finds in a graph, with the graphs it calls.
    pub(crate) fn check(&self, graph: &Graph) -> Result<(), String> {
        let mut graphs = self.graphs.clone();
        graphs.push(graph.clone());
        let document = wir::Document {
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

/// The names a statement defines, with the path of its op: a declaration or a call by its
/// name, a scatter or an `if` by the first name in its body.
pub(crate) type Placed = (String, Vec<(String, Shape)>);

pub(crate) fn place(document: &Analyzed, statement: &WorkflowStatement) -> Result<Placed, String> {
    Ok(match statement {
        WorkflowStatement::Declaration(decl) => {
            let name = decl.name().text().to_owned();
            let ty = declared(document, &decl.ty())?;
            let r#type = convert(&ty).map_err(|why| format!("{name}: {why}"))?;
            (name.clone(), vec![(name, Shape::Value(r#type))])
        }
        WorkflowStatement::Call(call) => {
            let unit = document::called(document, &call.target())?;
            let name = alias(call, &unit);
            let outputs = (boundary(&unit)?.1.into_iter())
                .map(|item| (item.name, item.r#type))
                .collect();
            (name.clone(), vec![(name, Shape::Call(outputs))])
        }
        WorkflowStatement::Scatter(scatter) => {
            let body: Vec<WorkflowStatement> = scatter.statements().collect();
            let defined = inner(document, &body, &array)?;
            let first = defined.first().map(|(name, _)| name.clone());
            let name = first.unwrap_or_else(|| scatter.variable().text().to_owned());
            (format!("scatter.{name}"), defined)
        }
        WorkflowStatement::Conditional(conditional) => {
            if conditional.clauses().count() > 1 {
                return Err("an else clause".into());
            }
            let body: Vec<WorkflowStatement> = conditional.if_clause().statements().collect();
            let defined = inner(document, &body, &optional)?;
            let first = defined.first().map(|(name, _)| name.clone());
            let name = first.unwrap_or_else(|| "empty".into());
            (format!("if.{name}"), defined)
        }
    })
}

/// The name of a call: its alias, else the name of what it calls.
fn alias(call: &CallStatement, unit: &Unit) -> String {
    match call.alias() {
        Some(alias) => alias.name().text().to_owned(),
        None => unit.name(),
    }
}

/// The value of a plain reference, like `name` or `call.output`.
pub(crate) fn direct(frame: &mut Frame, expr: &Expr) -> Option<Held> {
    match expr {
        Expr::Parenthesized(inner) => direct(frame, &inner.expr()),
        Expr::NameRef(name) => match frame.get(name.name().text())? {
            Bound::Value(held) => Some(held),
            Bound::Call(_) => None,
        },
        Expr::Access(access) => {
            let (target, member) = access.operands();
            let Expr::NameRef(name) = target else {
                return None;
            };
            match frame.get(name.name().text())? {
                Bound::Call(outputs) => (outputs.into_iter())
                    .find(|(output, _)| output == member.text())
                    .map(|(_, held)| held),
                Bound::Value(_) => None,
            }
        }
        _ => None,
    }
}

/// Where the value of an expression of type `want` comes from: a plain reference that fits,
/// else an expr op at `path`.
pub(crate) fn wire(
    frame: &mut Frame,
    expr: &Expr,
    want: &Type,
    path: &str,
) -> Result<Held, String> {
    if let Some(held) = direct(frame, expr).filter(|held| want.accepts(&held.r#type, &[])) {
        return Ok(held);
    }
    evaluated(frame, path, text(expr), expr, names(expr), want.clone())?;
    Ok(Held {
        end: end(Some(path), "value"),
        r#type: want.clone(),
    })
}

/// An expr op at `path` that computes `code` from the names it uses.
pub(crate) fn evaluated(
    frame: &mut Frame,
    path: &str,
    code: String,
    expr: &Expr,
    used: BTreeSet<String>,
    output: Type,
) -> Result<(), String> {
    if let Some(function) = reads(expr) {
        return Err(format!("{function} in the expression of {path}"));
    }
    let kind = Kind::Expr(wir::Expr {
        language: wir::ExprLanguage::Wdl,
        code,
        library: Vec::new(),
        scope: None,
        captures: Default::default(),
        form: None,
    });
    let mut op = blank(path, kind, frame.origin.clone());
    for name in used {
        let bound = frame
            .get(&name)
            .ok_or_else(|| format!("the name {name} in {path}"))?;
        for (item, held) in ports(&name, &bound) {
            op.inputs.push(port(&item, held.r#type));
            frame.edges.push(edge(held.end, end(Some(path), &item)));
        }
    }
    op.outputs = vec![port("value", output)];
    frame.ops.push(op);
    Ok(())
}
