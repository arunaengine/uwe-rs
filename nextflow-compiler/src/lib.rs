//! Compiles Nextflow pipelines into wir: what the build phase of the native driver wires up
//! becomes process ops with dialect tasks, stream ops with Groovy callbacks and call graphs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod graph;
mod groovy;
mod operators;
mod params;
mod process;
mod shapes;
mod sources;
mod start;

use std::path::{Path, PathBuf};

use nextflow_runtime::driver::{Nextflow, Settings};
use nextflow_runtime::export::{Kind, Network};
use serde_json::{Value as Json, json};
use workflow_graph::Launch;

pub use sources::Sources;

/// A compiled pipeline, and the reasons why it stays one native op when it does.
#[derive(Clone, Debug)]
pub struct Compilation {
    pub document: wir::Document,
    pub kept: Vec<Kept>,
}

/// A part of the pipeline that does not compile, which keeps the whole pipeline a native op.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Kept {
    pub path: String,
    pub reason: String,
}

impl Compilation {
    /// Whether no part of the pipeline stays native.
    pub fn full(&self) -> bool {
        self.kept.is_empty()
    }
}

/// What the dialect tasks of a compiled pipeline load: the profiles and config files of the
/// launch, and the folder that the document is meant for.
#[derive(Clone, Debug, Default)]
pub struct Context {
    pub profiles: Vec<String>,
    pub configs: Vec<PathBuf>,
    /// The named workflow of `-entry`, if one is selected.
    pub entry: Option<String>,
    pub base: PathBuf,
}

/// Compiles the pipeline of a launch, as the native driver with these settings would build it.
/// A build that needs what the compiler does not do gives one native op.
/// It fails only when the pipeline does not build.
pub fn compile(launch: &Launch, settings: Settings, base: &Path) -> Result<Compilation, String> {
    let context = Context {
        profiles: launch.profiles.clone(),
        configs: launch.configs.clone(),
        entry: launch.targets.first().cloned(),
        base: base.to_owned(),
    };
    let mut driver = Nextflow::new(launch, settings).map_err(|error| error.message)?;
    match driver.export() {
        Ok(network) => compile_network(&network, &context),
        Err(error) if error.unsupported => {
            let mut sources = Sources::new(&launch.target.to_string_lossy(), base);
            let kept = vec![Kept {
                path: "pipeline".into(),
                reason: error.message,
            }];
            // The params of the launch and config stay the inputs of the native op.
            let interface = driver.interface().map_err(|error| error.message)?;
            native(&interface, &context, &mut sources, kept)
        }
        Err(error) => Err(error.message),
    }
}

/// Compiles a built network; a node that does not compile keeps the pipeline one native op.
/// It fails only when the compiler makes no wir document, which is a bug of the compiler.
pub fn compile_network(network: &Network, context: &Context) -> Result<Compilation, String> {
    let mut sources = Sources::new(&network.script, &context.base);
    for config in &network.configs {
        sources.script(config);
    }
    sources.run(&network.params, &network.config);
    for (location, digest) in &network.reads {
        sources.data(location, digest);
    }
    for script in &network.scripts {
        sources.script(script);
    }
    let opaque = network.nodes.iter().filter_map(|node| match &node.kind {
        Kind::Opaque(reason) => Some(Kept {
            path: node.name.clone(),
            reason: reason.clone(),
        }),
        _ => None,
    });
    let kept: Vec<Kept> = opaque.collect();
    if !kept.is_empty() {
        return native(network, context, &mut sources, kept);
    }
    match graph::compile(network, context, &mut sources) {
        Ok(graphs) => {
            let hooks = hooks(network, &mut sources);
            Ok(Compilation {
                document: document(&sources, graphs, hooks)?,
                kept: Vec::new(),
            })
        }
        Err(kept) => native(network, context, &mut sources, vec![kept]),
    }
}

/// The pipeline as one native op, which runs it with the native driver; the params of the
/// run are inputs of the graph and of the op.
fn native(
    network: &Network,
    context: &Context,
    sources: &mut Sources,
    kept: Vec<Kept>,
) -> Result<Compilation, String> {
    let emits = &network.emits;
    let schema = network.schema.as_ref();
    // A native op takes typed params only: a type with `dynamic` in it, like the optional
    // dynamic of a null param, becomes the shape of the param's value, which may be null.
    let ports = params::ports(&network.params, schema);
    let inputs: Vec<Json> = (ports.into_iter())
        .map(|mut port| {
            if dynamic(&port["type"]) {
                port["type"] = optional(shape(&port["default"]));
            }
            port
        })
        .collect();
    let configs: Vec<String> = context
        .configs
        .iter()
        .map(|path| sources.config(path))
        .collect();
    let mut native = json!({"source": sources.main(), "language": "nextflow"});
    if let Some(entry) = &context.entry {
        native["entry"] = json!(entry);
    }
    if !context.profiles.is_empty() {
        native["profiles"] = json!(context.profiles);
    }
    if !configs.is_empty() {
        native["configs"] = json!(configs);
    }
    let outputs: Vec<Json> = (emits.iter())
        .map(|(name, _)| json!({"name": name, "type": "dynamic"}))
        .collect();
    let mut edges: Vec<Json> = (emits.iter())
        .map(|(name, _)| json!({"from": {"op": "pipeline", "port": name}, "to": {"port": name}}))
        .collect();
    edges.extend((inputs.iter()).map(|port| {
        let name = &port["name"];
        json!({"from": {"port": name}, "to": {"op": "pipeline", "port": name}})
    }));
    let op_inputs: Vec<Json> = (inputs.iter())
        .map(|port| json!({"name": port["name"], "type": port["type"]}))
        .collect();
    let op = json!({
        "path": "pipeline",
        "kind": {"native": native},
        "inputs": op_inputs,
        "outputs": outputs,
        "origin": {"source": sources.main()},
    });
    let graph =
        json!({"name": "main", "inputs": inputs, "outputs": outputs, "ops": [op], "edges": edges});
    Ok(Compilation {
        document: document(sources, vec![graph], Vec::new())?,
        kept,
    })
}

/// The run hooks: the observers of the run, then the `workflow.onComplete` and `onError`
/// handlers, which the capture `wirHook` marks for the Groovy bridge.
fn hooks(network: &Network, sources: &mut Sources) -> Vec<Json> {
    let mut hooks = observers(network, sources);
    for (on_error, callback) in &network.handlers {
        let kind = if *on_error { "error" } else { "complete" };
        let mut closure = groovy::closure(callback, sources);
        closure["captures"]["wirHook"] = json!({"value": kind});
        hooks.push(json!({kind: closure}));
    }
    hooks
}

/// The run hook that tells the observers of the run, like nf-prov, the processes of the
/// pipeline with the processes each feeds; scripts are named from the main script's folder.
fn observers(network: &Network, sources: &Sources) -> Vec<Json> {
    if network.processes.is_empty() {
        return Vec::new();
    }
    let processes: Vec<Json> = (network.processes.iter())
        .map(|process| {
            let mut observed = json!({"id": process.id, "name": process.name,
                "definition": process.definition, "feeds": process.feeds});
            if let Some(script) = &process.script {
                observed["script"] = json!(sources.beside_main(script));
            }
            observed
        })
        .collect();
    vec![json!({"observe": {"source": sources.main(), "processes": processes}})]
}

/// Whether a type is or holds `dynamic`.
fn dynamic(r#type: &Json) -> bool {
    match r#type {
        Json::String(name) => name == "dynamic",
        Json::Array(items) => items.iter().any(dynamic),
        Json::Object(object) => object.values().any(dynamic),
        _ => false,
    }
}

fn optional(r#type: Json) -> Json {
    match r#type.get("optional") {
        Some(_) => r#type,
        None => json!({"optional": r#type}),
    }
}

/// The wir type of the value of a param: scalars by their kind, arrays of the union of their
/// items, maps as records of optional fields, and null as an optional string.
fn shape(value: &Json) -> Json {
    match value {
        Json::Null => json!({"optional": "string"}),
        Json::Bool(_) => json!("bool"),
        Json::Number(number) if number.is_i64() => json!("int"),
        Json::Number(_) => json!("float"),
        Json::String(_) => json!("string"),
        Json::Array(items) => {
            let mut kinds: Vec<Json> = Vec::new();
            for kind in items.iter().map(shape) {
                if !kinds.contains(&kind) {
                    kinds.push(kind);
                }
            }
            let items = match kinds.len() {
                0 => json!("string"),
                1 => kinds.remove(0),
                _ => json!({"union": kinds}),
            };
            json!({"array": {"items": items}})
        }
        Json::Object(object) => {
            let fields = object
                .iter()
                .map(|(name, item)| json!({"name": name, "type": optional(shape(item))}));
            json!({"record": fields.collect::<Vec<_>>()})
        }
    }
}

/// The wir document of the graphs, which the strict format types check.
fn document(
    sources: &Sources,
    graphs: Vec<Json>,
    hooks: Vec<Json>,
) -> Result<wir::Document, String> {
    let mut document = json!({
        "wir_version": wir::VERSION,
        "sources": sources.list(),
        "graphs": graphs,
        "entry": "main",
    });
    if !hooks.is_empty() {
        document["hooks"] = Json::Array(hooks);
    }
    let parsed = serde_json::from_value(document);
    parsed.map_err(|error| format!("the compiler made no wir document: {error}"))
}
