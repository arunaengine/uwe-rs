//! Lowers CWL documents into wir: tools into task ops, workflows into graphs, and what has no
//! lowering yet into native CWL ops, so every document lowers, partly or fully.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod defaults;
mod expression;
mod group;
mod links;
mod outputs;
mod requirements;
mod runtime;
mod tool;
mod types;
mod workflow;

use std::path::{Path, PathBuf};

use cwl_parser::model::{Class, Process, Requirement, RequirementClass, short_name};
use group::Group;
use wir::{
    Document, Edge, Endpoint, Flow, Graph, Kind, Language, Native, Op, Origin, Port, Source, Type,
};

pub use tool::DEFAULT_IMAGE;

/// A lowered document, and the parts of it that stay native CWL ops.
#[derive(Clone, Debug)]
pub struct Lowering {
    pub document: Document,
    pub kept: Vec<Kept>,
}

/// An op that stays a native CWL op, and the first thing in it that has no lowering yet.
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

/// Lowers a loaded CWL document. Source locations are relative to `base`, the folder the wir
/// document is meant for, when they lie below it. It fails only when not even a native op can
/// hold the document, such as for an `Any` input of the whole document.
pub fn lower(document: &cwl_parser::Document, base: &Path) -> Result<Lowering, String> {
    let mut lowerer = Lowerer {
        base: base.to_owned(),
        sources: Vec::new(),
        kept: Vec::new(),
        graphs: Vec::new(),
        prefix: String::new(),
    };
    let process = &document.process;
    let scope = Scope::default().with(&process.requirements, &process.hints);
    let name = process_name(&process.id);
    let main = match &process.class {
        Class::Workflow(workflow) => match lowerer.workflow(process, workflow, &scope, "main") {
            Ok(graph) => graph,
            Err(reason) => {
                let op = lowerer.native(process, &name, reason, &scope)?;
                single(process, one(op), &scope)?
            }
        },
        _ => {
            let op = lowerer.op(process, &Scope::default(), &name)?;
            single(process, op, &scope)?
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
    })
}

/// The entry graph of a process that the ops of a group run, with its ports.
fn single(process: &Process, group: Group, scope: &Scope) -> Result<Graph, String> {
    let (inputs, outputs) = boundary(process, scope)?;
    let name = group.main().path.clone();
    let mut edges = group.edges.clone();
    for port in &inputs {
        for (op, input) in group.takers(&port.name) {
            edges.push(edge((None, &port.name), (Some(&op), &input)));
        }
    }
    for port in &outputs {
        edges.push(edge((Some(&name), &port.name), (None, &port.name)));
    }
    Ok(Graph {
        name: "main".into(),
        inputs,
        outputs,
        ops: group.ops,
        edges,
    })
}

fn one(op: Op) -> Group {
    Group::of(vec![op])
}

/// The requirements and hints in effect, outer ones first, as uwe's CWL driver keeps them.
#[derive(Clone, Debug, Default)]
pub(crate) struct Scope {
    pub requirements: Vec<Requirement>,
    pub hints: Vec<Requirement>,
}

impl Scope {
    /// The scope inside a process or step with these requirements and hints.
    fn with(&self, requirements: &[Requirement], hints: &[Requirement]) -> Scope {
        let mut inner = self.clone();
        inner.requirements.extend(requirements.iter().cloned());
        inner.hints.extend(hints.iter().cloned());
        inner
    }

    /// The `expressionLib` of the innermost `InlineJavascriptRequirement`.
    pub fn library(&self) -> Vec<String> {
        (self.requirements.iter().rev())
            .find_map(|entry| match &entry.class {
                RequirementClass::InlineJavascript { expression_lib } => {
                    Some(expression_lib.clone())
                }
                _ => None,
            })
            .unwrap_or_default()
    }

    /// The innermost requirement that `pick` takes, else the innermost hint.
    pub fn find<'s, T>(&'s self, pick: impl Fn(&'s RequirementClass) -> Option<T>) -> Option<T> {
        (self.requirements.iter().rev())
            .find_map(|entry| pick(&entry.class))
            .or_else(|| self.hints.iter().rev().find_map(|entry| pick(&entry.class)))
    }
}

struct Lowerer {
    base: PathBuf,
    sources: Vec<Source>,
    kept: Vec<Kept>,
    /// The graphs that call ops run, apart from the entry graph.
    graphs: Vec<Graph>,
    /// The path of the call whose graph is being lowered, like `inner/`, for the kept ops.
    prefix: String,
}

impl Lowerer {
    /// The ops that run a process, the last at `path`: lowered where it can be, else a
    /// native op.
    fn op(&mut self, process: &Process, outer: &Scope, path: &str) -> Result<Group, String> {
        let scope = outer.with(&process.requirements, &process.hints);
        let (source, entry) = self.source(process);
        let origin = Some(Origin {
            source: source.clone(),
            at: entry.clone(),
        });
        let lowered = match &process.class {
            Class::CommandLineTool(tool) => tool::lower(process, tool, &scope, &self.base)
                .and_then(|lowered| {
                    let mut op = blank(path, Kind::Task(lowered.task), origin.clone());
                    (op.inputs, op.outputs) = (lowered.inputs, lowered.outputs);
                    op.attributes = lowered.attributes;
                    let library = scope.library();
                    let group = group::computed(op, &lowered.values, &library);
                    match lowered.evaluated.is_empty() {
                        true => Ok(group),
                        false => self.evaluated(process, group, &lowered.evaluated, &library, path),
                    }
                }),
            Class::Workflow(workflow) => self.call(process, workflow, &scope, path).map(one),
            Class::ExpressionTool(tool) => self.expression(process, tool, &scope, path).map(one),
            Class::Operation => Err("this process class has no lowering yet".into()),
        };
        match lowered {
            Ok(mut group) => {
                if let Some(main) = group.ops.last_mut() {
                    main.origin = origin;
                }
                Ok(group)
            }
            Err(reason) => self.native(process, path, reason, &scope).map(one),
        }
    }

    /// A native op at `path` that runs a process in `scope`, kept for `reason`.
    fn native(
        &mut self,
        process: &Process,
        path: &str,
        reason: String,
        scope: &Scope,
    ) -> Result<Op, String> {
        let (source, entry) = self.source(process);
        let origin = Some(Origin {
            source: source.clone(),
            at: entry.clone(),
        });
        if entry.as_deref().is_some_and(|entry| entry.contains('/')) {
            return Err(format!(
                "{path}: {reason}, and its inline process cannot run alone"
            ));
        }
        let (inputs, outputs) = boundary(process, scope)?;
        if (inputs.iter().chain(&outputs)).any(|port| dynamic(&port.r#type)) {
            return Err(format!(
                "{path}: {reason}, and a native block cannot take Any"
            ));
        }
        self.kept.push(Kept {
            path: format!("{}{path}", self.prefix),
            reason,
        });
        Ok(Op {
            path: path.to_owned(),
            kind: Kind::Native(Native {
                source,
                language: Language::Cwl,
                entry,
                profiles: Vec::new(),
                configs: Vec::new(),
            }),
            inputs,
            outputs,
            after: Vec::new(),
            effects: Vec::new(),
            attributes: Default::default(),
            origin,
        })
    }

    /// The id of the source that holds a process, added once per file, and the process's
    /// fragment in that file.
    fn source(&mut self, process: &Process) -> (String, Option<String>) {
        let (file, fragment) = match process.id.split_once('#') {
            Some((file, fragment)) => (file, Some(fragment.to_owned())),
            None => (process.id.as_str(), None),
        };
        let location = match cwl_parser::iri::file_path(file) {
            Some(path) => match path.strip_prefix(&self.base) {
                Ok(relative) => relative.to_string_lossy().into_owned(),
                Err(_) => path.to_string_lossy().into_owned(),
            },
            None => file.to_owned(),
        };
        if let Some(known) = self.sources.iter().find(|item| item.location == location) {
            return (known.id.clone(), fragment);
        }
        let stem = process_name(file);
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
            language: Language::Cwl,
            location,
            version: Some(process.version.to_string()),
            digest: None,
        });
        (id, fragment)
    }
}

/// The ports of a process as a whole: an input with a default may be null.
fn boundary(process: &Process, scope: &Scope) -> Result<(Vec<Port>, Vec<Port>), String> {
    let named = types::named(scope);
    let port = |name: &str, r#type: Type| Port {
        name: short_name(name).to_owned(),
        r#type,
        flow: Flow::Value,
        default: None,
    };
    let mut inputs = Vec::new();
    for input in &process.inputs {
        let r#type = types::convert(&input.value_type, &named)
            .map_err(|why| format!("input {}: {why}", short_name(&input.id)))?;
        let r#type = match input.default {
            Some(_) => types::optional(r#type),
            None => r#type,
        };
        inputs.push(port(&input.id, r#type));
    }
    let mut outputs = Vec::new();
    for output in &process.outputs {
        let r#type = types::convert(&output.value_type, &named)
            .map_err(|why| format!("output {}: {why}", short_name(&output.id)))?;
        outputs.push(port(&output.id, r#type));
    }
    Ok((inputs, outputs))
}

fn dynamic(r#type: &Type) -> bool {
    match r#type {
        Type::Dynamic => true,
        Type::Optional(inner) => dynamic(inner),
        Type::Array(array) => dynamic(&array.items),
        Type::Map(map) => dynamic(&map.value),
        Type::Record(fields) => fields.iter().any(|field| dynamic(&field.r#type)),
        Type::Union(items) | Type::Tuple(items) => items.iter().any(dynamic),
        _ => false,
    }
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

fn edge(from: (Option<&str>, &str), to: (Option<&str>, &str)) -> Edge {
    let end = |(op, port): (Option<&str>, &str)| Endpoint {
        op: op.map(str::to_owned),
        port: port.to_owned(),
    };
    Edge {
        from: end(from),
        to: end(to),
    }
}

/// The name uwe's CWL driver gives the task of a process run alone: the fragment of its id,
/// else the file name without its extension.
pub fn process_name(id: &str) -> String {
    if let Some((_, fragment)) = id.split_once('#') {
        return short_name(fragment).to_owned();
    }
    let name = id.trim_end_matches('/').rsplit('/').next().unwrap_or(id);
    match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem.to_owned(),
        _ => name.to_owned(),
    }
}
