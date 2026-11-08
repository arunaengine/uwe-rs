//! Compiles Argo workflows into wir: steps and DAG templates into graphs, and their steps and
//! tasks into Argo dialect tasks, or the whole workflow into one native op where it needs more.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub mod depends;
mod entry;
mod graphs;
mod sites;
pub mod tags;

use std::path::Path;

use argo_parser::decode;
use argo_parser::model::Template;
use argo_runtime::adapter::Adapter;
use wir::{
    ArrayType, Attributes, Document, Edge, Endpoint, FileType, Flow, Kind, Language, Op, Origin,
    Port, Source, Subscription, Type, ValueRules,
};
use workflow_graph::Launch;

/// The id of the source of the workflow in the compiled document.
pub const SOURCE: &str = "workflow";

/// The entry graph, which takes the workflow parameters and gives the outputs of the
/// entrypoint by their names.
pub const ENTRY: &str = "workflow";

/// A compiled workflow, and the part of it that stays a native Argo op.
#[derive(Clone, Debug)]
pub struct Compiled {
    pub document: Document,
    pub kept: Vec<Kept>,
}

/// An op that stays a native Argo op, and the first thing in it that wir cannot hold yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Kept {
    pub path: String,
    pub reason: String,
}

impl Compiled {
    /// Whether no part of the workflow stays native.
    pub fn full(&self) -> bool {
        self.kept.is_empty()
    }
}

/// Compiles the Argo workflow of a file, with the template documents of `configs`. Source
/// locations are relative to `base` when they lie below it. It fails only when not even a
/// native op can hold the workflow.
pub fn compile(
    path: &Path,
    configs: &[std::path::PathBuf],
    base: &Path,
) -> Result<Compiled, String> {
    let launch = Launch {
        target: path.to_owned(),
        configs: configs.to_vec(),
        simulated: true,
        ..Launch::default()
    };
    let services = argo_runtime::Services::default();
    let adapter = Adapter::load(&launch, services).map_err(|error| error.message)?;
    let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
    let location = match absolute.strip_prefix(base) {
        Ok(relative) => relative.to_string_lossy().into_owned(),
        Err(_) => absolute.to_string_lossy().into_owned(),
    };
    let source = Source {
        id: SOURCE.into(),
        language: Language::Argo,
        location,
        version: None,
        digest: Some(task_model::Digest::sha256(&bytes).to_string()),
    };
    let mut compiler = graphs::Compiler::new(adapter);
    let (graphs, kept) = match compiler.workflow() {
        Ok(graphs) => (graphs, Vec::new()),
        Err(kept) => (vec![compiler.native()?], vec![kept]),
    };
    let document = Document {
        wir_version: wir::VERSION.into(),
        sources: vec![source],
        types: Vec::new(),
        graphs,
        entry: ENTRY.into(),
    };
    if let Err(problems) = wir::verify(&document) {
        let problems: Vec<String> = problems.iter().map(ToString::to_string).collect();
        return Err(format!(
            "the compiled document does not verify: {}",
            problems.join("; ")
        ));
    }
    Ok(Compiled { document, kept })
}

/// The typed template of a template node.
fn typed(node: &argo_parser::Node) -> Result<Template, String> {
    decode::<Template>(node)
        .map(|(template, _)| template)
        .map_err(|error| error.to_string())
}

fn port(name: impl Into<String>, r#type: Type) -> Port {
    Port {
        name: name.into(),
        r#type,
        flow: Flow::Value,
        default: None,
    }
}

fn with_default(mut port: Port, value: serde_json::Value) -> Port {
    port.default = Some(value);
    port
}

/// The type of a parameter: a text.
fn text() -> Type {
    Type::String
}

/// The type of an artifact: a File or a Directory, or null when it is not there.
fn artifact() -> Type {
    let either = Type::Union(vec![Type::File(FileType::default()), Type::Directory]);
    Type::Optional(Box::new(either))
}

fn array(items: Type) -> Type {
    Type::Array(ArrayType {
        items: Box::new(items),
        nonempty: false,
    })
}

/// The type of a node name in parts, which the dialect tasks join.
fn path_type() -> Type {
    array(Type::String)
}

fn op(path: impl Into<String>, kind: Kind) -> Op {
    Op {
        path: path.into(),
        kind,
        inputs: Vec::new(),
        outputs: Vec::new(),
        after: Vec::new(),
        depends: None,
        handles: Vec::new(),
        effects: Vec::new(),
        attributes: Attributes::default(),
        origin: None,
        values: ValueRules::default(),
    }
}

fn origin(node: &argo_parser::Node) -> Option<Origin> {
    Some(Origin {
        source: SOURCE.into(),
        at: Some(node.position.to_string()),
    })
}

fn end(op: Option<&str>, port: &str) -> Endpoint {
    Endpoint {
        op: op.map(str::to_owned),
        port: port.to_owned(),
    }
}

fn edge(from: Endpoint, to: Endpoint) -> Edge {
    Edge {
        from,
        to,
        read: Subscription::default(),
    }
}

/// Names that a graph or op takes from a template, made safe for wir.
fn safe(name: &str) -> String {
    (name.chars())
        .map(
            |c| match c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') {
                true => c,
                false => '_',
            },
        )
        .collect()
}
