//! The tool definition of a recorded tool: file inputs and outputs as ports with the EDAM
//! formats of their endings, and the other inputs as parameters.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::{Map, Value as Json, json};
use tool_definition::{Curation, Definition, Image, InputPort, OutputFormat, OutputPort, PortKind};

use crate::command::name;
use crate::tool::{Kind, Tool};

impl Tool {
    /// The definition of the tool, named like the file `record` writes. It names no source;
    /// `record` names the CWL file it writes beside it.
    pub fn to_definition(&self) -> Definition {
        let id = name(&self.base_command);
        let mut inputs = Vec::new();
        let mut properties = Map::new();
        for input in &self.inputs {
            match port_kind(input.kind) {
                Some(kind) => inputs.push(InputPort {
                    name: input.id.clone(),
                    title: None,
                    description: None,
                    kind,
                    cardinality: Default::default(),
                    formats: (input.default.as_ref())
                        .and_then(|value| value["location"].as_str())
                        .and_then(tool_definition::formats::ending)
                        .map(str::to_owned)
                        .into_iter()
                        .collect(),
                    unit: None,
                    required: true,
                    default: input.default.clone(),
                    examples: Vec::new(),
                    conditions: Vec::new(),
                    active_when: None,
                }),
                None => {
                    let mut schema = json!({"type": json_type(input.kind)});
                    if let Some(default) = &input.default {
                        schema["default"] = default.clone();
                    }
                    properties.insert(input.id.clone(), schema);
                }
            }
        }
        let outputs = (self.outputs.iter())
            .filter_map(|output| {
                let kind = port_kind(output.kind)?;
                // A glob that names an input, like `$(inputs.out)`, ends as its value does.
                let named = (output.glob.strip_prefix("$(inputs."))
                    .and_then(|rest| rest.strip_suffix(')'))
                    .and_then(|id| self.inputs.iter().find(|input| input.id == id))
                    .and_then(|input| input.default.as_ref()?.as_str());
                let glob = named.unwrap_or(&output.glob);
                let format = (output.format.as_deref())
                    .map(|format| format.trim_start_matches("edam:").to_owned())
                    .or_else(|| tool_definition::formats::ending(glob).map(str::to_owned));
                Some(OutputPort {
                    name: output.id.clone(),
                    title: None,
                    description: None,
                    kind,
                    cardinality: Default::default(),
                    format: OutputFormat::Terms(format.into_iter().collect()),
                    guarantees: Vec::new(),
                    created_when: None,
                    optional: false,
                    schema: Json::Null,
                })
            })
            .collect();
        let parameters = match properties.is_empty() {
            true => Json::Null,
            false => json!({"type": "object", "properties": properties}),
        };
        Definition {
            title: id.clone(),
            id,
            version: "1.0".to_owned(),
            description: Some(format!("Recorded from `{}`", self.base_command.join(" "))),
            source: None,
            image: (self.image.clone()).map(|name| Image { name, digest: None }),
            license: None,
            authors: Vec::new(),
            citations: Vec::new(),
            biotools: None,
            operations: Vec::new(),
            topics: Vec::new(),
            keywords: Vec::new(),
            inputs,
            parameters,
            outputs,
            resources: Default::default(),
            tests: Vec::new(),
            curation: Curation::Unchecked,
        }
    }
}

fn port_kind(kind: Kind) -> Option<PortKind> {
    match kind {
        Kind::File => Some(PortKind::File),
        Kind::Directory => Some(PortKind::Directory),
        _ => None,
    }
}

fn json_type(kind: Kind) -> &'static str {
    match kind {
        Kind::Int | Kind::Long => "integer",
        Kind::Float => "number",
        Kind::Boolean => "boolean",
        _ => "string",
    }
}
