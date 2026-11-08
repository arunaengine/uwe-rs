//! Lowers WDL documents into wir: workflows into graphs, tasks into expr and task ops, and what
//! has no lowering yet into native WDL ops, so every document lowers, partly or fully.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod bodies;
mod inputs;
mod library;
mod outputs;
mod task;
mod types;
mod workflow;

use std::path::{Path, PathBuf};

use wdl_analysis::Document as Analyzed;
use wdl_ast::v1::Expr;
use wdl_ast::{AstNode, AstToken};
use wdl_runtime::document::{Callable, Unit};
use wdl_runtime::expression::{self, Environment};
use wir::{Document, Edge, Endpoint, Graph, Kind, Language, Native, Op, Origin, Port, Source};

pub use wdl_runtime::task::DEFAULT_IMAGE;

/// A lowered document, and the parts of it that stay native WDL ops.
#[derive(Clone, Debug)]
pub struct Lowering {
    pub document: Document,
    pub kept: Vec<Kept>,
    /// The name of the workflow or task that the entry graph runs, which input files use as
    /// the prefix of their keys.
    pub name: String,
}

/// An op that stays a native WDL op, and the first thing in it that has no lowering yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Kept {
    pub path: String,
    pub reason: String,
}

impl Lowering {
    /// Whether no part of the document stays native.
    pub fn full(&self) -> bool {
        self.kept.is_empty()
    }
}

/// Lowers the workflow or task `entry` of a WDL file, else the one a run would pick. Source
/// locations are relative to `base`, the folder the wir document is meant for, when they lie
/// below it. It fails only when not even a native op can hold the document.
pub fn lower(path: &Path, entry: Option<&str>, base: &Path) -> Result<Lowering, String> {
    let document = wdl_runtime::document::load(path).map_err(|error| error.message)?;
    let unit = wdl_runtime::document::entry(&document, entry).map_err(|error| error.message)?;
    let mut lowerer = Lowerer {
        base: base.to_owned(),
        sources: Vec::new(),
        kept: Vec::new(),
        graphs: Vec::new(),
        prefix: String::new(),
        done: Vec::new(),
        fallbacks: Vec::new(),
    };
    let name = unit.name();
    let main = match &unit.callable {
        Callable::Workflow(_) => match lowerer.workflow(&unit, "main") {
            Ok(graph) => graph,
            Err(reason) => {
                let op = lowerer.native(&unit, &name, reason)?;
                single(&unit, op)?
            }
        },
        Callable::Task(_) => {
            let op = lowerer.task(&unit, &name)?;
            single(&unit, op)?
        }
    };
    let mut graphs = vec![main];
    graphs.append(&mut lowerer.graphs);
    let document = Document {
        wir_version: wir::VERSION.into(),
        sources: lowerer.sources,
        types: Vec::new(),
        graphs,
        entry: "main".into(),
    };
    if let Err(problems) = wir::verify(&document) {
        let problems: Vec<String> = problems.iter().map(ToString::to_string).collect();
        return Err(format!(
            "the lowered document does not verify: {}",
            problems.join("; ")
        ));
    }
    Ok(Lowering {
        document,
        kept: lowerer.kept,
        name,
    })
}

/// The entry graph that runs one op with the ports of a callable.
fn single(unit: &Unit, op: Op) -> Result<Graph, String> {
    let (inputs, outputs) = boundary(unit)?;
    let mut edges = Vec::new();
    for port in inputs
        .iter()
        .filter(|port| op.inputs.iter().any(|p| p.name == port.name))
    {
        edges.push(edge(end(None, &port.name), end(Some(&op.path), &port.name)));
    }
    for port in &outputs {
        edges.push(edge(end(Some(&op.path), &port.name), end(None, &port.name)));
    }
    Ok(Graph {
        name: "main".into(),
        inputs,
        outputs,
        ops: vec![op],
        edges,
    })
}

struct Lowerer {
    base: PathBuf,
    sources: Vec<Source>,
    kept: Vec<Kept>,
    /// The graphs that call ops run, apart from the entry graph.
    graphs: Vec<Graph>,
    /// The path of the call whose graph is being lowered, like `inner/`, for the kept ops.
    prefix: String,
    /// The graph of each task and workflow lowered so far, by document and name, or why it
    /// has none, so that every call of it shares one graph.
    done: Vec<((String, String), Result<String, String>)>,
    /// The literal defaults of optional inputs, by graph and port. A call op holds them for
    /// the inputs that it is not given, so that a given `None` stays `None`, as in WDL.
    fallbacks: Vec<(String, String, serde_json::Value)>,
}

impl Lowerer {
    /// A native op at `path` that runs a task or workflow as a whole, kept for `reason`.
    fn native(&mut self, unit: &Unit, path: &str, reason: String) -> Result<Op, String> {
        let (inputs, outputs) = boundary(unit)?;
        if (inputs.iter().chain(&outputs)).any(|port| types::open(&port.r#type)) {
            return Err(format!(
                "{path}: {reason}, and a native block cannot take an Object"
            ));
        }
        self.kept.push(Kept {
            path: format!("{}{path}", self.prefix),
            reason,
        });
        let source = self.source(&unit.document);
        let kind = Kind::Native(Native {
            source: source.clone(),
            language: Language::Wdl,
            entry: Some(unit.name()),
            profiles: Vec::new(),
            configs: Vec::new(),
        });
        let mut op = blank(path, kind, origin(&source, &unit.name()));
        (op.inputs, op.outputs) = (inputs, outputs);
        Ok(op)
    }

    /// The id of the source of a document, added once per file.
    fn source(&mut self, document: &Analyzed) -> String {
        let file = document.uri().to_file_path().ok();
        let location = match &file {
            Some(path) => match path.strip_prefix(&self.base) {
                Ok(relative) => relative.to_string_lossy().into_owned(),
                Err(_) => path.to_string_lossy().into_owned(),
            },
            None => document.uri().to_string(),
        };
        if let Some(known) = self.sources.iter().find(|item| item.location == location) {
            return known.id.clone();
        }
        let stem = (file.as_ref().and_then(|path| path.file_stem()))
            .map_or("wdl".into(), |stem| stem.to_string_lossy().into_owned());
        let mut id: String = (stem.chars())
            .map(
                |c| match c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') {
                    true => c,
                    false => '_',
                },
            )
            .collect();
        if id.is_empty() || self.sources.iter().any(|item| item.id == id) {
            id = format!("{id}-{}", self.sources.len() + 1);
        }
        self.sources.push(Source {
            id: id.clone(),
            language: Language::Wdl,
            location,
            version: document.version().map(|version| version.to_string()),
            digest: None,
        });
        id
    }

    /// A graph name from `stem` that no graph has yet.
    fn graph_name(&self, stem: &str) -> String {
        let mut name = stem.to_owned();
        while name == "main" || self.graphs.iter().any(|graph| graph.name == name) {
            name = format!("{stem}-{}", self.graphs.len() + 1);
        }
        name
    }

    /// The graph of a task or workflow from an earlier call, or why it has none.
    fn known(&self, unit: &Unit) -> Option<Result<String, String>> {
        let key = (unit.document.uri().to_string(), unit.name());
        (self.done.iter().find(|(known, _)| *known == key)).map(|(_, graph)| graph.clone())
    }

    fn remember(&mut self, unit: &Unit, graph: Result<String, String>) {
        let key = (unit.document.uri().to_string(), unit.name());
        self.done.push((key, graph));
    }

    /// A call op at `path` of a lowered graph, with the ports of that graph.
    fn call(&self, graph: &str, path: &str, origin: Option<Origin>) -> Op {
        let called = self.graphs.iter().find(|item| item.name == graph);
        let mut op = blank(
            path,
            Kind::Call(wir::Call {
                graph: graph.into(),
            }),
            origin,
        );
        if let Some(called) = called {
            (op.inputs, op.outputs) = (called.inputs.clone(), called.outputs.clone());
        }
        for (_, input, value) in self.fallbacks.iter().filter(|item| item.0 == graph) {
            if let Some(port) = op.inputs.iter_mut().find(|port| port.name == *input) {
                port.default = Some(value.clone());
            }
        }
        op
    }
}

/// The ports of a task or workflow as a whole: an input that need not be given is optional.
fn boundary(unit: &Unit) -> Result<(Vec<Port>, Vec<Port>), String> {
    let name = unit.name();
    let (inputs, outputs) = match &unit.callable {
        Callable::Task(_) => {
            let found = unit.document.task_by_name(&name);
            let task = found.ok_or_else(|| format!("the task {name} was not analyzed"))?;
            (
                task.inputs().as_ref().clone(),
                task.outputs().as_ref().clone(),
            )
        }
        Callable::Workflow(_) => {
            let found = unit.document.workflow_by_name(&name);
            let workflow = found.ok_or_else(|| format!("the workflow {name} was not analyzed"))?;
            (
                workflow.inputs().as_ref().clone(),
                workflow.outputs().as_ref().clone(),
            )
        }
    };
    let mut ports = Vec::new();
    for (input, item) in &inputs {
        let r#type = types::convert(item.ty()).map_err(|why| format!("input {input}: {why}"))?;
        let r#type = match item.required() {
            true => r#type,
            false => types::optional(r#type),
        };
        ports.push(types::port(input, r#type));
    }
    let mut results = Vec::new();
    for (output, item) in &outputs {
        let r#type = types::convert(item.ty()).map_err(|why| format!("output {output}: {why}"))?;
        results.push(types::port(output, r#type));
    }
    Ok((ports, results))
}

/// The value of an expression that uses no names, like the literal default of an input.
fn constant(expr: &Expr) -> Option<wdl_runtime::Value> {
    struct Empty;

    impl Environment for Empty {
        fn lookup(&self, _: &str) -> Option<wdl_runtime::Value> {
            None
        }
    }

    expression::evaluate(expr, &mut Empty).ok()
}

/// The standard functions that read files, which an `expr` op must not call, and those that
/// write files, which only its command section may call.
const READS: [&str; 14] = [
    "read_string",
    "read_int",
    "read_float",
    "read_boolean",
    "read_lines",
    "read_tsv",
    "read_map",
    "read_json",
    "read_object",
    "read_objects",
    "size",
    "glob",
    "stdout",
    "stderr",
];
const WRITES: [&str; 6] = [
    "write_lines",
    "write_tsv",
    "write_map",
    "write_json",
    "write_object",
    "write_objects",
];

/// The first function in a node that reads or writes files.
fn reads<N: AstNode<wdl_ast::SyntaxNode>>(node: &N) -> Option<String> {
    called(node, &READS).or_else(|| called(node, &WRITES))
}

/// The first function of `names` that a node calls.
fn called<N: AstNode<wdl_ast::SyntaxNode>>(node: &N, names: &[&str]) -> Option<String> {
    (node.descendants::<wdl_ast::v1::CallExpr>())
        .map(|call| call.target().text().to_owned())
        .find(|name| names.contains(&name.as_str()))
}

/// The source text of a node.
fn text<N: AstNode<wdl_ast::SyntaxNode>>(node: &N) -> String {
    node.inner().text().to_string()
}

fn origin(source: &str, at: &str) -> Option<Origin> {
    Some(Origin {
        source: source.to_owned(),
        at: Some(at.to_owned()),
    })
}

/// An op without ports, dependencies or attributes.
fn blank(path: &str, kind: Kind, origin: Option<Origin>) -> Op {
    Op {
        path: path.to_owned(),
        kind,
        inputs: Vec::new(),
        outputs: Vec::new(),
        after: Vec::new(),
        effects: Vec::new(),
        attributes: Default::default(),
        origin,
    }
}

fn edge(from: Endpoint, to: Endpoint) -> Edge {
    Edge { from, to }
}

fn end(op: Option<&str>, port: &str) -> Endpoint {
    Endpoint {
        op: op.map(str::to_owned),
        port: port.to_owned(),
    }
}
