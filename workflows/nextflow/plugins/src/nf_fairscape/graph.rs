//! The EVI inverse links, the inputs and outputs, and the evidence graph of nf-fairscape's crate.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;

use super::{COMPUTATION, DATASET, reference, refs};
use crate::report_format::Json;

/// The EVI relations and their inverses, in the order the plugin applies them.
const INVERSES: [(&str, &str); 18] = [
    ("associateFor", "associatedWith"),
    ("challengedBy", "challenges"),
    ("containedBy", "contains"),
    ("created", "createdBy"),
    ("datasetUsedBy", "usedDataset"),
    ("derivedFrom", "derivedTo"),
    ("describedBy", "describes"),
    ("directlyChallengedBy", "directlyChallenges"),
    ("directlySupportedBy", "directlySupports"),
    ("distributedFrom", "hasDistribution"),
    ("generated", "generatedBy"),
    ("indirectlyChallengedBy", "indirectlyChallenges"),
    ("packagedBy", "packages"),
    ("representedBy", "represents"),
    ("serviceUsedBy", "usedService"),
    ("softwareUsedBy", "usedSoftware"),
    ("supportedBy", "supports"),
    ("used", "usedBy"),
];

/// Adds the inverse of each EVI relation that one entity states to the entity it names.
pub(super) fn link(entities: &mut [Json]) {
    let ids: Vec<Option<String>> = (entities.iter())
        .map(|entity| entity.get("@id")?.as_str().map(str::to_owned))
        .collect();
    for (one, other) in INVERSES {
        for (from, to) in [(one, other), (other, one)] {
            let mut additions: Vec<(usize, String)> = Vec::new();
            for (place, entity) in entities.iter().enumerate() {
                let Some(id) = &ids[place] else {
                    continue;
                };
                for target in referenced(entity.get(from)) {
                    if let Some(found) = ids.iter().position(|it| it.as_deref() == Some(&target)) {
                        additions.push((found, id.clone()));
                    }
                }
            }
            for (place, id) in additions {
                let entity = &mut entities[place];
                if entity.get(to).is_none() {
                    entity.insert(to, Json::List(Vec::new()));
                }
                if let Some(Json::List(items)) = entity.get_mut(to)
                    && !referenced_list(items).contains(&id)
                {
                    items.push(reference(&id));
                }
            }
        }
    }
}

/// The inputs and outputs of the run: Datasets that nothing generated, and Datasets that no
/// Computation used, without the files that are part of something else.
pub(super) fn ends(entities: &[Json]) -> (Vec<String>, Vec<String>) {
    let mut inputs = Vec::new();
    let mut outputs = Vec::new();
    for entity in entities.iter().filter(|entity| is(entity, DATASET[1])) {
        let Some(id) = entity.get("@id").and_then(Json::as_str) else {
            continue;
        };
        if referenced(entity.get("generatedBy")).is_empty() {
            inputs.push(id.to_owned());
        }
        if referenced(entity.get("datasetUsedBy")).is_empty() && entity.get("isPartOf").is_none() {
            outputs.push(id.to_owned());
        }
    }
    (inputs, outputs)
}

/// `provenance-graph.json`: the entities that lead to the outputs, each after the entities
/// it came from, then the root with its outputs.
pub(super) fn evidence(entities: &[Json], root: &str, outputs: &[String], name: &str) -> Json {
    let by_id: HashMap<&str, &Json> = (entities.iter())
        .filter_map(|entity| Some((entity.get("@id")?.as_str()?, entity)))
        .collect();
    let run = entities
        .get(2)
        .and_then(|it| it.get("@id")?.as_str())
        .unwrap_or_default();
    let mut nodes: Vec<(String, Json)> = Vec::new();
    for output in outputs {
        visit(output, &by_id, run, &mut nodes);
    }
    let mut root_node = Json::Object(Vec::new());
    if let Some(entity) = by_id.get(root) {
        for key in ["@id", "@type", "name", "description"] {
            if let Some(value) = entity.get(key) {
                root_node.insert(key, value.clone());
            }
        }
    }
    root_node.insert("hasOutputs", refs(outputs));
    nodes.push((root.to_owned(), root_node));
    let count = nodes.len() as u64;
    let mut listed: Vec<String> = outputs.to_vec();
    listed.push(root.to_owned());
    let suffix = root.split_once('/').map_or(root, |(_, rest)| rest);
    let naan = root.split_once('/').map_or("", |(naan, _)| naan);
    Json::Object(Vec::new())
        .with("@type", "evi:EvidenceGraph")
        .with("@id", format!("{naan}/evidence-graph-{suffix}"))
        .with("owner", root)
        .with("description", format!("Evidence graph for {name}"))
        .with("name", format!("Evidence Graph - {name}"))
        .with("outputs", refs(&listed))
        .with("@graph", Json::Object(nodes))
        .with(
            "condensation_stats",
            Json::Object(Vec::new())
                .with("condensed", false)
                .with("originalEntityCount", count)
                .with("condensedEntityCount", count)
                .with("datasetGroupCount", 0u64),
        )
}

/// Adds an entity of the evidence graph after those it came from: a Dataset after the task
/// that made it, a Computation after its Datasets and Software. The run is left out.
fn visit(id: &str, by_id: &HashMap<&str, &Json>, run: &str, nodes: &mut Vec<(String, Json)>) {
    if id == run || nodes.iter().any(|(known, _)| known == id) {
        return;
    }
    let Some(entity) = by_id.get(id) else {
        return;
    };
    let mut node = Json::Object(Vec::new());
    for key in ["@id", "@type", "name", "description"] {
        if let Some(value) = entity.get(key) {
            node.insert(key, value.clone());
        }
    }
    // Marks the entity as seen before its sources, so that a cycle ends.
    nodes.push((id.to_owned(), Json::Null));
    if is(entity, DATASET[1]) {
        let producer = referenced(entity.get("generatedBy"))
            .into_iter()
            .find(|it| it != run);
        if let Some(producer) = producer {
            visit(&producer, by_id, run, nodes);
            node.insert("generatedBy", reference(&producer));
        }
    } else if is(entity, COMPUTATION[1]) {
        for key in ["usedDataset", "usedSoftware"] {
            let sources = referenced(entity.get(key));
            for source in &sources {
                visit(source, by_id, run, nodes);
            }
            node.insert(key, refs(&sources));
        }
    }
    nodes.retain(|(known, _)| known != id);
    nodes.push((id.to_owned(), node));
}

/// The ids that a relation names, as a list of references or one reference.
fn referenced(value: Option<&Json>) -> Vec<String> {
    match value {
        Some(Json::List(items)) => referenced_list(items),
        Some(single @ Json::Object(_)) => referenced_list(std::slice::from_ref(single)),
        _ => Vec::new(),
    }
}

fn referenced_list(items: &[Json]) -> Vec<String> {
    let ids = items.iter().filter_map(|item| item.get("@id")?.as_str());
    ids.map(str::to_owned).collect()
}

fn is(entity: &Json, kind: &str) -> bool {
    match entity.get("@type") {
        Some(Json::List(kinds)) => kinds.iter().any(|it| it.as_str() == Some(kind)),
        _ => false,
    }
}
