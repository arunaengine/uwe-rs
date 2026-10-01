//! Describes published workflow outputs or emitted main-script channels as JSON Schema.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use input_schema::{DRAFT, PATH};
use nextflow_evaluator::lowered::Module;
use serde_json::{Map, Value as Json, json};

use crate::OutputTarget;

/// The schema of the outputs of a main script, with the targets of its `output { }` block below
/// `output_dir`.
pub fn schema(module: &Module, targets: &[OutputTarget], output_dir: &str, title: &str) -> Json {
    let mut properties = Map::new();
    for target in targets {
        properties.insert(target.name.to_string(), published(target, output_dir));
    }
    if targets.is_empty() {
        for workflow in &module.workflows {
            let Some(name) = &workflow.name else {
                continue;
            };
            for (emit, _) in &workflow.emits {
                let description = format!("a channel that the workflow {name} emits");
                (properties.entry(emit.to_string()))
                    .or_insert_with(|| json!({"description": description}));
            }
        }
    }
    json!({"$schema": DRAFT, "title": title, "type": "object", "properties": properties})
}

/// The schema of an output of the `output { }` block: the files that the pipeline publishes, and
/// where.
fn published(target: &OutputTarget, output_dir: &str) -> Json {
    let folder = output_dir.trim_end_matches('/');
    let mut description = match &target.path {
        Some(path) => format!(
            "the files of the output `{}`, published to `{folder}/{}`",
            target.name,
            path.trim_start_matches("./")
        ),
        None => format!(
            "the files of the output `{}`, published below `{folder}` where the workflow chooses \
             for each value",
            target.name
        ),
    };
    if let Some(index) = &target.index {
        description.push_str(&format!("; `{folder}/{index}` lists them"));
    }
    json!({
        "type": "array",
        "items": {"type": "string", "format": PATH},
        "description": description
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_output_targets() {
        let targets = [
            OutputTarget {
                name: "multiqc".into(),
                path: Some("reports/multiqc".into()),
                index: None,
            },
            OutputTarget {
                name: "samples".into(),
                path: None,
                index: Some("samples.csv".into()),
            },
        ];
        let schema = schema(&Module::default(), &targets, "results/", "demo");
        assert_eq!(
            schema["properties"]["multiqc"]["description"],
            "the files of the output `multiqc`, published to `results/reports/multiqc`"
        );
        assert_eq!(
            schema["properties"]["samples"]["description"],
            "the files of the output `samples`, published below `results` where the workflow \
             chooses for each value; `results/samples.csv` lists them"
        );
        assert_eq!(input_schema::valid(&schema), Ok(()));
    }
}
