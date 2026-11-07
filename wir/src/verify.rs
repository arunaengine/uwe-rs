//! The verifier: checks the version, names and types of a document.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeSet;

use crate::document::{Document, Graph, Port, Source, VERSION};
use crate::op::{Kind, Op};
use crate::types::{Type, TypeDef};

/// One problem and where it is: `document`, `sources/<id>`, `types/<name>`, `<graph>` or
/// `<graph>/<op path>`.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{at}: {problem}")]
pub struct VerifyError {
    pub at: String,
    pub problem: Problem,
}

/// What is wrong.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Problem {
    #[error("wir version {0} is not known; this verifier knows {VERSION}")]
    Version(String),
    #[error(
        "{0:?} is not a valid name: use letters, digits, '_', '-' and '.', paths join them by '/'"
    )]
    BadName(String),
    #[error("{0} is defined twice")]
    Duplicate(String),
    #[error("no graph named {0}")]
    NoGraph(String),
    #[error("no source named {0}")]
    NoSource(String),
    #[error("no type named {0}")]
    NoType(String),
    #[error("type {0} is defined through itself")]
    RecursiveType(String),
    #[error("{0} must not be empty")]
    Empty(String),
    #[error("no op {0} in this graph")]
    NoOp(String),
}

/// Checks a document. It returns every problem found, or nothing when the document is valid.
pub fn verify(document: &Document) -> Result<(), Vec<VerifyError>> {
    let mut check = Checker {
        document,
        defs: &document.types,
        errors: Vec::new(),
    };
    if document.wir_version != VERSION {
        check.report("document", Problem::Version(document.wir_version.clone()));
    }
    check.sources();
    // Type checks below recurse through definitions, so they need definitions without cycles.
    if check.definitions() {
        check.graphs();
    }
    if check.errors.is_empty() {
        Ok(())
    } else {
        Err(check.errors)
    }
}

struct Checker<'d> {
    document: &'d Document,
    defs: &'d [TypeDef],
    errors: Vec<VerifyError>,
}

impl<'d> Checker<'d> {
    fn report(&mut self, at: &str, problem: Problem) {
        self.errors.push(VerifyError {
            at: at.to_owned(),
            problem,
        });
    }

    fn sources(&mut self) {
        let ids = self.document.sources.iter().map(|item| item.id.as_str());
        self.names("document", ids, false);
    }

    /// Checks the named types and returns whether they are free of cycles.
    fn definitions(&mut self) -> bool {
        self.names(
            "document",
            self.defs.iter().map(|item| item.name.as_str()),
            false,
        );
        let mut acyclic = true;
        for def in self.defs {
            let at = format!("types/{}", def.name);
            self.check_type(&at, &def.r#type);
            let next = |name: &str| self.named(name).map(named_in).unwrap_or_default();
            if reaches(&def.name, next) {
                self.report(&at, Problem::RecursiveType(def.name.clone()));
                acyclic = false;
            }
        }
        acyclic
    }

    fn graphs(&mut self) {
        let document = self.document;
        self.names(
            "document",
            document.graphs.iter().map(|item| item.name.as_str()),
            false,
        );
        if self.graph(&document.entry).is_none() {
            self.report("document", Problem::NoGraph(document.entry.clone()));
        }
        for graph in &document.graphs {
            self.graph_body(graph);
        }
    }

    fn graph_body(&mut self, graph: &'d Graph) {
        let at = graph.name.as_str();
        self.ports(at, "graph input", &graph.inputs);
        self.ports(at, "graph output", &graph.outputs);
        self.names(at, graph.ops.iter().map(|op| op.path.as_str()), true);
        for op in &graph.ops {
            self.op(graph, op);
        }
    }

    fn op(&mut self, graph: &Graph, op: &'d Op) {
        let at = format!("{}/{}", graph.name, op.path);
        self.ports(&at, "input", &op.inputs);
        self.ports(&at, "output", &op.outputs);
        for dependency in &op.after {
            if !graph.ops.iter().any(|item| item.path == dependency.op) {
                self.report(&at, Problem::NoOp(dependency.op.clone()));
            }
            if dependency.on.is_empty() {
                self.report(
                    &at,
                    Problem::Empty(format!("the statuses of {}", dependency.op)),
                );
            }
        }
        if let Some(origin) = &op.origin {
            self.source(&at, &origin.source);
        }
        for name in called_by(&op.kind) {
            if self.graph(name).is_none() {
                self.report(&at, Problem::NoGraph(name.to_owned()));
            }
        }
    }

    fn ports(&mut self, at: &str, side: &str, ports: &[Port]) {
        self.names(at, ports.iter().map(|port| port.name.as_str()), false);
        for port in ports {
            self.check_type(&format!("{at} {side} {}", port.name), &port.r#type);
        }
    }

    /// Reports invalid names and names used twice; `paths` allows segments joined by `/`.
    fn names<'n>(&mut self, at: &str, names: impl Iterator<Item = &'n str>, paths: bool) {
        let mut seen = BTreeSet::new();
        for name in names {
            let valid = match paths {
                true => name.split('/').all(valid_name),
                false => valid_name(name),
            };
            if !valid {
                self.report(at, Problem::BadName(name.to_owned()));
            }
            if !seen.insert(name) {
                self.report(at, Problem::Duplicate(name.to_owned()));
            }
        }
    }

    fn check_type(&mut self, at: &str, r#type: &Type) {
        let mut problems = Vec::new();
        walk(r#type, &mut |item| match item {
            Type::Named(name) if self.named(name).is_none() => {
                problems.push(Problem::NoType(name.clone()));
            }
            Type::Union(items) | Type::Tuple(items) if items.is_empty() => {
                problems.push(Problem::Empty(show(item)));
            }
            Type::Enum(symbols) if symbols.is_empty() => problems.push(Problem::Empty(show(item))),
            Type::Record(fields) => {
                let mut seen = BTreeSet::new();
                for field in fields.iter().filter(|field| !seen.insert(&field.name)) {
                    problems.push(Problem::Duplicate(format!("field {}", field.name)));
                }
            }
            _ => {}
        });
        problems
            .into_iter()
            .for_each(|problem| self.report(at, problem));
    }

    fn source(&mut self, at: &str, id: &str) -> Option<&'d Source> {
        let document = self.document;
        let found = document.sources.iter().find(|item| item.id == id);
        if found.is_none() {
            self.report(at, Problem::NoSource(id.to_owned()));
        }
        found
    }

    fn named(&self, name: &str) -> Option<&'d Type> {
        self.defs
            .iter()
            .find(|item| item.name == name)
            .map(|item| &item.r#type)
    }

    fn graph(&self, name: &str) -> Option<&'d Graph> {
        self.document.graphs.iter().find(|item| item.name == name)
    }
}

fn called_by(kind: &Kind) -> Vec<&str> {
    match kind {
        Kind::Call(call) => vec![&call.graph],
        Kind::Map(map) => vec![&map.body],
        Kind::Filter(filter) => vec![&filter.body],
        Kind::Branch(branch) => vec![&branch.body],
        Kind::Loop(body) => vec![&body.body],
        _ => Vec::new(),
    }
}

fn named_in(r#type: &Type) -> Vec<&str> {
    let mut names = Vec::new();
    walk(r#type, &mut |item| {
        if let Type::Named(name) = item {
            names.push(name.as_str());
        }
    });
    names
}

/// Whether `start` can be reached again by following `next` from it.
fn reaches<'n>(start: &str, next: impl Fn(&str) -> Vec<&'n str>) -> bool {
    let mut stack = next(start);
    let mut seen = BTreeSet::new();
    while let Some(name) = stack.pop() {
        if name == start {
            return true;
        }
        if seen.insert(name) {
            stack.extend(next(name));
        }
    }
    false
}

fn walk<'t>(r#type: &'t Type, visit: &mut dyn FnMut(&'t Type)) {
    visit(r#type);
    match r#type {
        Type::Record(fields) => fields.iter().for_each(|field| walk(&field.r#type, visit)),
        Type::Array(array) => walk(&array.items, visit),
        Type::Map(map) => walk(&map.value, visit),
        Type::Optional(inner) => walk(inner, visit),
        Type::Union(items) | Type::Tuple(items) => items.iter().for_each(|item| walk(item, visit)),
        _ => {}
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && (name.chars()).all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

fn show(value: &impl serde::Serialize) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

#[cfg(test)]
#[path = "verify_tests.rs"]
mod tests;
