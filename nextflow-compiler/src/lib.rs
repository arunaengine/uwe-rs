//! Compiles Nextflow pipelines into wir: what the build phase of the native driver wires up
//! becomes process ops with dialect tasks, stream ops with Groovy callbacks and call graphs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod graph;
mod groovy;
mod operators;
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
    pub document: Json,
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
/// A build that needs what the compiler does not do, like reading data, gives one native op.
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
        Ok(network) => Ok(compile_network(&network, &context)),
        Err(error) if error.unsupported => {
            let mut sources = Sources::new(&launch.target.to_string_lossy(), base);
            let kept = vec![Kept {
                path: "pipeline".into(),
                reason: error.message,
            }];
            Ok(native(&[], &context, &mut sources, kept))
        }
        Err(error) => Err(error.message),
    }
}

/// Compiles a built network; a node that does not compile keeps the pipeline one native op.
pub fn compile_network(network: &Network, context: &Context) -> Compilation {
    let mut sources = Sources::new(&network.script, &context.base);
    let opaque = network.nodes.iter().filter_map(|node| match &node.kind {
        Kind::Opaque(reason) => Some(Kept {
            path: node.name.clone(),
            reason: reason.clone(),
        }),
        _ => None,
    });
    let kept: Vec<Kept> = opaque.collect();
    if !kept.is_empty() {
        return native(&network.emits, context, &mut sources, kept);
    }
    match graph::compile(network, context, &mut sources) {
        Ok(graphs) => Compilation {
            document: document(&sources, context, graphs),
            kept: Vec::new(),
        },
        Err(kept) => native(&network.emits, context, &mut sources, vec![kept]),
    }
}

/// The pipeline as one native op, which runs it with the native driver.
fn native(
    emits: &[(String, usize)],
    context: &Context,
    sources: &mut Sources,
    kept: Vec<Kept>,
) -> Compilation {
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
    let edges: Vec<Json> = (emits.iter())
        .map(|(name, _)| json!({"from": {"op": "pipeline", "port": name}, "to": {"port": name}}))
        .collect();
    let op = json!({
        "path": "pipeline",
        "kind": {"native": native},
        "outputs": outputs,
        "origin": {"source": sources.main()},
    });
    let graph = json!({"name": "main", "outputs": outputs, "ops": [op], "edges": edges});
    Compilation {
        document: document(sources, context, vec![graph]),
        kept,
    }
}

fn document(sources: &Sources, _context: &Context, graphs: Vec<Json>) -> Json {
    json!({
        "wir_version": "0.1",
        "sources": sources.list(),
        "graphs": graphs,
        "entry": "main",
    })
}
