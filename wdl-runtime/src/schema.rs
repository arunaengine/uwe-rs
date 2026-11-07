//! The inputs and outputs of a WDL workflow or task as JSON Schemas, and input values from
//! an input file and the command line.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::Path;

use indexmap::IndexMap;
use input_schema::{DIRECTORY_PATH, DRAFT, FILE_PATH};
use serde_json::{Map as JsonMap, Value as Json, json};
use wdl_analysis::types::{CompoundType, CustomType, Optional, PrimitiveType, Type};

use crate::document::{Callable, Unit};
use crate::value::{Value, located};

/// The type of each input, and whether it needs a value.
pub type Types = IndexMap<String, (Type, bool)>;

/// What the input file and the command line give.
pub struct Given {
    /// The values of the inputs by name.
    pub values: BTreeMap<String, Value>,
    /// The values of the inputs of calls of the workflow, by call name.
    pub calls: BTreeMap<String, BTreeMap<String, Value>>,
}

/// The type of each input of a task or workflow, and whether it needs a value.
pub fn input_types(unit: &Unit) -> Types {
    let name = unit.name();
    let inputs = match &unit.callable {
        Callable::Task(_) => unit.document.task_by_name(&name).map(|task| task.inputs()),
        Callable::Workflow(_) => unit
            .document
            .workflow_by_name(&name)
            .map(|workflow| workflow.inputs()),
    };
    (inputs
        .into_iter()
        .flat_map(|inputs| inputs.as_ref().clone()))
    .map(|(name, input)| (name, (input.ty().clone(), input.required())))
    .collect()
}

/// The type of each output of a task or workflow.
pub fn output_types(unit: &Unit) -> IndexMap<String, Type> {
    let name = unit.name();
    let outputs = match &unit.callable {
        Callable::Task(_) => unit.document.task_by_name(&name).map(|task| task.outputs()),
        Callable::Workflow(_) => unit
            .document
            .workflow_by_name(&name)
            .map(|workflow| workflow.outputs()),
    };
    (outputs
        .into_iter()
        .flat_map(|outputs| outputs.as_ref().clone()))
    .map(|(name, output)| (name, output.ty().clone()))
    .collect()
}

/// The JSON Schema of the inputs: one property per input, by its name without the workflow.
pub fn inputs(unit: &Unit) -> Json {
    let mut properties = JsonMap::new();
    let mut required = Vec::new();
    for (name, (ty, needed)) in input_types(unit) {
        let mut schema = type_schema(&ty);
        if let Some(fields) = schema.as_object_mut().filter(|_| has_file(&ty)) {
            exists(fields);
        }
        if needed {
            required.push(Json::String(name.clone()));
        }
        properties.insert(name, schema);
    }
    object(unit, properties, required)
}

/// The JSON Schema of the outputs of a run, by output name.
pub fn outputs(unit: &Unit) -> Json {
    let mut properties = JsonMap::new();
    let mut required = Vec::new();
    for (name, ty) in output_types(unit) {
        if !ty.is_optional() {
            required.push(Json::String(name.clone()));
        }
        properties.insert(name, type_schema(&ty));
    }
    object(unit, properties, required)
}

fn object(unit: &Unit, properties: JsonMap<String, Json>, required: Vec<Json>) -> Json {
    let mut schema = json!({
        "$schema": DRAFT,
        "title": unit.name(),
        "type": "object",
        "properties": properties,
    });
    if !required.is_empty() {
        schema["required"] = Json::Array(required);
    }
    schema
}

/// Inputs must name files that exist; outputs name files that the run makes.
fn exists(fields: &mut JsonMap<String, Json>) {
    if matches!(
        fields.get("format").and_then(Json::as_str),
        Some(FILE_PATH | DIRECTORY_PATH)
    ) {
        fields.insert("exists".into(), Json::Bool(true));
    }
    for value in fields.values_mut() {
        if let Some(inner) = value.as_object_mut() {
            exists(inner);
        }
    }
}

fn has_file(ty: &Type) -> bool {
    let text = ty.to_string();
    text.contains("File") || text.contains("Directory")
}

/// The JSON Schema of a WDL type; an optional member of a compound type may be null.
fn type_schema(ty: &Type) -> Json {
    let nullable = |schema: Json, ty: &Type| match ty.is_optional() {
        true => json!({"anyOf": [schema, {"type": "null"}]}),
        false => schema,
    };
    match ty {
        Type::Primitive(primitive, _) => match primitive {
            PrimitiveType::Boolean => json!({"type": "boolean"}),
            PrimitiveType::Integer => json!({"type": "integer"}),
            PrimitiveType::Float => json!({"type": "number"}),
            PrimitiveType::String => json!({"type": "string"}),
            PrimitiveType::File => json!({"type": "string", "format": FILE_PATH}),
            PrimitiveType::Directory => json!({"type": "string", "format": DIRECTORY_PATH}),
        },
        Type::Compound(CompoundType::Array(array), _) => {
            let element = array.element_type();
            let mut schema =
                json!({"type": "array", "items": nullable(type_schema(element), element)});
            if array.is_non_empty() {
                schema["minItems"] = json!(1);
            }
            schema
        }
        Type::Compound(CompoundType::Pair(pair), _) => json!({
            "type": "object",
            "properties": {
                "left": nullable(type_schema(pair.left_type()), pair.left_type()),
                "right": nullable(type_schema(pair.right_type()), pair.right_type()),
            },
            "required": ["left", "right"],
        }),
        Type::Compound(CompoundType::Map(map), _) => json!({
            "type": "object",
            "additionalProperties": nullable(type_schema(map.value_type()), map.value_type()),
        }),
        Type::Compound(CompoundType::Custom(CustomType::Struct(record)), _) => {
            let mut properties = JsonMap::new();
            let mut required = Vec::new();
            for (name, member) in record.members() {
                if !member.is_optional() {
                    required.push(Json::String(name.clone()));
                }
                properties.insert(name.clone(), nullable(type_schema(member), member));
            }
            json!({"type": "object", "title": record.name().as_str(), "properties": properties, "required": required})
        }
        _ => json!({}),
    }
}

/// The values of the inputs, from the input file and then the command line, by input name;
/// keys `workflow.call.input` of the input file go to the calls of the workflow, by call name.
pub fn given(
    unit: &Unit,
    file: Option<&Path>,
    params: &[(String, String)],
    calls: &dyn Fn(&str) -> Option<Types>,
) -> Result<Given, String> {
    let types = input_types(unit);
    let prefix = format!("{}.", unit.name());
    let mut values = BTreeMap::new();
    let mut nested: BTreeMap<String, BTreeMap<String, Value>> = BTreeMap::new();
    if let Some(file) = file {
        let text = std::fs::read_to_string(file)
            .map_err(|error| format!("cannot read {}: {error}", file.display()))?;
        let json: Json =
            serde_json::from_str(&text).map_err(|error| format!("{}: {error}", file.display()))?;
        let Json::Object(fields) = json else {
            return Err(format!("{} must hold a JSON object", file.display()));
        };
        let base = std::path::absolute(file).unwrap_or(file.to_owned());
        let base = base.parent().unwrap_or(Path::new("/"));
        for (key, json) in fields {
            let name = key.strip_prefix(&prefix).unwrap_or(&key);
            if let Some((ty, _)) = types.get(name) {
                let value = Value::from_json(&json, ty, base)
                    .map_err(|error| format!("input `{name}`: {error}"))?;
                values.insert(name.to_owned(), value);
                continue;
            }
            let Some((call, input)) = name.split_once('.') else {
                return Err(format!("{} has no input `{name}`", unit.name()));
            };
            let ty = calls(call).and_then(|inputs| inputs.get(input).map(|(ty, _)| ty.clone()));
            let ty = ty.ok_or_else(|| format!("{} has no input `{name}`", unit.name()))?;
            let value = Value::from_json(&json, &ty, base)
                .map_err(|error| format!("input `{name}`: {error}"))?;
            nested
                .entry(call.to_owned())
                .or_default()
                .insert(input.to_owned(), value);
        }
    }
    let mut texts: Vec<(String, Vec<String>)> = Vec::new();
    for (name, text) in params {
        let name = name.strip_prefix(&prefix).unwrap_or(name).to_owned();
        match texts.iter_mut().find(|(known, _)| *known == name) {
            Some((_, list)) => list.push(text.clone()),
            None => texts.push((name, vec![text.clone()])),
        }
    }
    let base = std::env::current_dir().unwrap_or_default();
    for (name, list) in texts {
        let (ty, _) = types
            .get(&name)
            .ok_or_else(|| format!("{} has no input `{name}`", unit.name()))?;
        let value = parameter(ty, &list, &base).map_err(|error| format!("--{name}: {error}"))?;
        values.insert(name, value);
    }
    Ok(Given {
        values,
        calls: nested,
    })
}

/// The value of an input given on the command line; repeated values make an array.
fn parameter(ty: &Type, texts: &[String], base: &Path) -> Result<Value, String> {
    if let Type::Compound(CompoundType::Array(array), _) = ty {
        let element = array.element_type();
        let items = (texts.iter()).map(|text| parameter(element, std::slice::from_ref(text), base));
        return Value::Array(items.collect::<Result<_, _>>()?).coerce(ty);
    }
    let text = texts.last().map(String::as_str).unwrap_or_default();
    let value = match ty.as_primitive() {
        Some(PrimitiveType::String) => Value::String(text.to_owned()),
        Some(PrimitiveType::File) => Value::File(located(text, base)),
        Some(PrimitiveType::Directory) => Value::Directory(located(text, base)),
        _ => {
            let json = serde_json::from_str(text).unwrap_or(Json::String(text.to_owned()));
            return Value::from_json(&json, ty, base);
        }
    };
    value.coerce(ty)
}
