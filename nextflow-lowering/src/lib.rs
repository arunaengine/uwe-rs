//! Lowers Nextflow pipelines into wir: what the build phase of the native driver wires up
//! becomes process ops with dialect tasks, stream ops with Groovy callbacks and call graphs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod sources;

use std::path::{Path, PathBuf};

use nextflow_runtime::driver::{Nextflow, Settings};
use nextflow_runtime::export::{Kind, Network};
use serde_json::{Value as Json, json};
use workflow_graph::Launch;

pub use sources::Sources;

/// A lowered pipeline, and the reasons why it stays one native op when it does.
#[derive(Clone, Debug)]
pub struct Lowering {
    pub document: Json,
    pub kept: Vec<Kept>,
}

/// A part of the pipeline without a lowering, which keeps the whole pipeline a native op.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Kept {
    pub path: String,
    pub reason: String,
}

impl Lowering {
    /// Whether no part of the pipeline stays native.
    pub fn full(&self) -> bool {
        self.kept.is_empty()
    }
}

/// What the dialect tasks of a lowered pipeline load: the profiles and config files of the
/// launch, and the folder that the document is meant for.
#[derive(Clone, Debug, Default)]
pub struct Context {
    pub profiles: Vec<String>,
    pub configs: Vec<PathBuf>,
    /// The named workflow of `-entry`, if one is selected.
    pub entry: Option<String>,
    pub base: PathBuf,
}

/// Lowers the pipeline of a launch, as the native driver with these settings would build it.
/// It fails only when the pipeline does not build.
pub fn lower(launch: &Launch, settings: Settings, base: &Path) -> Result<Lowering, String> {
    let mut driver = Nextflow::new(launch, settings).map_err(|error| error.message)?;
    let network = driver.export().map_err(|error| error.message)?;
    let context = Context {
        profiles: launch.profiles.clone(),
        configs: launch.configs.clone(),
        entry: launch.targets.first().cloned(),
        base: base.to_owned(),
    };
    Ok(lower_network(&network, &context))
}

/// Lowers a built network. This version keeps every node native, with the reason why.
pub fn lower_network(network: &Network, context: &Context) -> Lowering {
    let mut sources = Sources::new(&network.script, &context.base);
    let kept = (network.nodes.iter())
        .map(|node| Kept {
            path: node.name.clone(),
            reason: match &node.kind {
                Kind::Opaque(reason) => reason.clone(),
                _ => "a node that the lowering does not cover yet".into(),
            },
        })
        .collect();
    native(network, context, &mut sources, kept)
}

/// The pipeline as one native op, which runs it with the native driver.
fn native(
    network: &Network,
    context: &Context,
    sources: &mut Sources,
    kept: Vec<Kept>,
) -> Lowering {
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
    let outputs: Vec<Json> = (network.emits.iter())
        .map(|(name, _)| json!({"name": name, "type": "dynamic"}))
        .collect();
    let edges: Vec<Json> = (network.emits.iter())
        .map(|(name, _)| json!({"from": {"op": "pipeline", "port": name}, "to": {"port": name}}))
        .collect();
    let op = json!({
        "path": "pipeline",
        "kind": {"native": native},
        "outputs": outputs,
        "origin": {"source": sources.main()},
    });
    let graph = json!({"name": "main", "outputs": outputs, "ops": [op], "edges": edges});
    Lowering {
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
