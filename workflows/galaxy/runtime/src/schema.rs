//! The JSON Schemas (draft 2020-12) of the inputs and outputs of Galaxy tools and workflows,
//! for `uwe inputs`, forms and the check of a run before any task starts.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use galaxy_parser::tool::Tool;
use serde_json::{Map, Value as Json, json};

use crate::workflow::{InputKind, Workflow};

const DRAFT: &str = "https://json-schema.org/draft/2020-12/schema";

/// The inputs of a job of `tool`: its params nested as in the tool, data inputs as paths of
/// files that must exist.
pub fn tool_inputs(tool: &Tool) -> Json {
    let mut schema = galaxy_parser::definition::inputs_schema(tool);
    exists(&mut schema);
    titled(schema, &tool.name, tool.description.as_deref())
}

/// The outputs of a run of `tool`: each output as the path of the file it made. Outputs with
/// a filter may be missing.
pub fn tool_outputs(tool: &Tool) -> Json {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for output in &tool.outputs {
        let file = file(output.label.as_deref());
        let schema = match output.kind.as_str() {
            "collection" => json!({"type": "array", "items": file}),
            _ => file,
        };
        if output.filters.is_empty() {
            required.push(Json::String(output.name.clone()));
        }
        properties.insert(output.name.clone(), schema);
    }
    let schema = object(properties, required);
    titled(schema, &tool.name, tool.description.as_deref())
}

/// The inputs of a workflow by label: datasets as paths, collections as lists of paths and
/// parameters by their type.
pub fn workflow_inputs(workflow: &Workflow) -> Json {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for input in &workflow.inputs {
        let mut schema = match &input.kind {
            InputKind::Dataset => json!({"type": "string", "format": "file-path", "exists": true}),
            InputKind::Collection => json!({"type": "array",
                "items": {"type": "string", "format": "file-path", "exists": true}}),
            InputKind::Parameter(kind) => match kind.as_str() {
                "integer" => json!({"type": "integer"}),
                "float" => json!({"type": "number"}),
                "boolean" => json!({"type": "boolean"}),
                _ => json!({"type": "string"}),
            },
        };
        if let Some(text) = &input.annotation {
            schema["description"] = Json::String(text.clone());
        }
        if let Some(default) = &input.default {
            schema["default"] = default.clone();
        }
        if !input.optional && input.default.is_none() {
            required.push(Json::String(input.label.clone()));
        }
        properties.insert(input.label.clone(), schema);
    }
    let schema = object(properties, required);
    titled(schema, &workflow.name, workflow.annotation.as_deref())
}

/// The outputs of a workflow by label, as the paths of the files the run made.
pub fn workflow_outputs(workflow: &Workflow) -> Json {
    let properties: Map<String, Json> = (workflow.outputs.iter())
        .map(|output| (output.label.clone(), file(None)))
        .collect();
    let required = (workflow.outputs.iter())
        .map(|output| Json::String(output.label.clone()))
        .collect();
    let schema = object(properties, required);
    titled(schema, &workflow.name, workflow.annotation.as_deref())
}

fn file(title: Option<&str>) -> Json {
    let mut schema = json!({"type": "string", "format": "file-path"});
    if let Some(title) = title.filter(|title| !title.contains('$')) {
        schema["title"] = Json::String(title.to_owned());
    }
    schema
}

fn object(properties: Map<String, Json>, required: Vec<Json>) -> Json {
    let mut schema = json!({"type": "object", "properties": properties});
    if !required.is_empty() {
        schema["required"] = Json::Array(required);
    }
    schema
}

fn titled(mut schema: Json, title: &str, description: Option<&str>) -> Json {
    schema["$schema"] = Json::String(DRAFT.to_owned());
    if !title.is_empty() {
        schema["title"] = Json::String(title.to_owned());
    }
    if let Some(text) = description {
        schema["description"] = Json::String(text.to_owned());
    }
    schema
}

/// Inputs must name files that exist.
fn exists(schema: &mut Json) {
    let Json::Object(fields) = schema else {
        if let Json::Array(items) = schema {
            items.iter_mut().for_each(exists);
        }
        return;
    };
    if fields.get("format").and_then(Json::as_str) == Some("file-path") {
        fields.insert("exists".to_owned(), Json::Bool(true));
    }
    for value in fields.values_mut() {
        exists(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_as_paths() {
        let text = r#"<tool id="t" name="T"><inputs>
            <param name="reads" type="data" format="fastqsanger" label="Reads"/>
            <param name="mates" type="data" multiple="true" optional="true"/>
            <param name="count" type="integer" value="2"/></inputs>
            <outputs><data name="out" format="txt"/>
            <data name="log" format="txt"><filter>count &gt; 1</filter></data></outputs></tool>"#;
        let tool = galaxy_parser::tool::parse(text, "t.xml", &mut |_| unreachable!("no imports"))
            .expect("a tool");
        let inputs = tool_inputs(&tool);
        assert_eq!(inputs["properties"]["reads"]["format"], "file-path");
        assert_eq!(inputs["properties"]["reads"]["exists"], true);
        assert_eq!(
            inputs["properties"]["mates"]["type"],
            json!(["array", "null"])
        );
        assert_eq!(inputs["required"], json!(["reads"]));
        let outputs = tool_outputs(&tool);
        assert_eq!(outputs["required"], json!(["out"]));
    }
}
