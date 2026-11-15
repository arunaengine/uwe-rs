//! Converts a Galaxy tool into a tool definition: data ports, parameter schema and outputs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use serde_json::{Map, Value as Json, json};
use tool_definition::{
    Cardinality, Citation, Curation, Definition, InputPort, OutputFormat, OutputPort, PortKind,
    Source,
};

use crate::tool::{Input, Param, ParamKind, Tool};

/// The definition of `tool`. `formats` maps Galaxy datatypes to EDAM formats; a port with a
/// datatype it lacks takes every format.
pub fn convert(
    tool: &Tool,
    formats: &BTreeMap<String, String>,
    source: Option<Source>,
) -> Definition {
    let profile = tool.profile.as_deref();
    let mut inputs = Vec::new();
    ports(&tool.inputs, "", &[], formats, profile, &mut inputs);
    let parameters = match object(&tool.inputs, profile) {
        schema if schema["properties"].as_object().is_some_and(Map::is_empty) => Json::Null,
        schema => schema,
    };
    Definition {
        id: tool.id.clone(),
        version: tool.version.clone(),
        title: tool.name.clone(),
        description: tool.description.clone(),
        source,
        image: None,
        license: tool.license.clone(),
        authors: Vec::new(),
        citations: (tool.dois.iter())
            .map(|doi| Citation {
                text: format!("doi:{doi}"),
                doi: Some(doi.clone()),
            })
            .collect(),
        biotools: tool.biotools.clone(),
        operations: Vec::new(),
        topics: Vec::new(),
        inputs,
        parameters,
        outputs: tool
            .outputs
            .iter()
            .map(|output| {
                let format = match (&output.format_source, &output.format) {
                    (Some(input), _) => OutputFormat::Follows(input.clone()),
                    (None, Some(format)) => {
                        OutputFormat::Terms(terms(std::slice::from_ref(format), formats))
                    }
                    (None, None) => OutputFormat::default(),
                };
                OutputPort {
                    name: output.name.clone(),
                    title: None,
                    description: None,
                    kind: PortKind::File,
                    cardinality: match output.kind.as_str() {
                        "collection" => Cardinality::List,
                        _ => Cardinality::One,
                    },
                    format,
                    guarantees: Vec::new(),
                    created_when: match output.filters.as_slice() {
                        [] => None,
                        [one] => Some(one.clone()),
                        many => Some(
                            many.iter()
                                .map(|f| format!("({f})"))
                                .collect::<Vec<_>>()
                                .join(" and "),
                        ),
                    },
                }
            })
            .collect(),
        resources: Default::default(),
        tests: Vec::new(),
        curation: Curation::Unchecked,
    }
}

/// One branch test on the way to an input: the path of the conditional, its test param, the
/// case value as JSON, and whether it is the default case.
#[derive(Clone)]
struct Branch {
    path: Vec<String>,
    test: String,
    value: Json,
    default: bool,
}

fn ports(
    inputs: &[Input],
    prefix: &str,
    branches: &[Branch],
    formats: &BTreeMap<String, String>,
    profile: Option<&str>,
    found: &mut Vec<InputPort>,
) {
    for input in inputs {
        match input {
            Input::Param(param)
                if matches!(param.kind, ParamKind::Data | ParamKind::Collection) =>
            {
                let list = param.multiple || param.kind == ParamKind::Collection;
                found.push(InputPort {
                    name: format!("{prefix}{}", param.name),
                    title: param.label.clone(),
                    description: param.help.clone(),
                    kind: PortKind::File,
                    cardinality: if list {
                        Cardinality::List
                    } else {
                        Cardinality::One
                    },
                    formats: terms(&param.formats, formats),
                    unit: None,
                    required: !param.optional,
                    default: None,
                    examples: Vec::new(),
                    conditions: Vec::new(),
                    active_when: condition(branches),
                });
            }
            Input::Param(_) => {}
            Input::Section(section) => {
                let inner = format!("{prefix}{}|", section.name);
                ports(&section.inputs, &inner, branches, formats, profile, found);
            }
            Input::Repeat(repeat) => {
                let start = found.len();
                let inner = format!("{prefix}{}|", repeat.name);
                ports(&repeat.inputs, &inner, branches, formats, profile, found);
                for port in &mut found[start..] {
                    port.cardinality = Cardinality::List;
                    port.required = false;
                }
            }
            Input::Conditional(conditional) => {
                let path: Vec<String> = (prefix.split('|'))
                    .filter(|part| !part.is_empty())
                    .map(str::to_owned)
                    .chain([conditional.name.clone()])
                    .collect();
                let default = case_value(&conditional.test, &conditional.test.initial(profile));
                for case in &conditional.cases {
                    let mut inner = branches.to_vec();
                    inner.push(Branch {
                        path: path.clone(),
                        test: conditional.test.name.clone(),
                        value: when_value(&conditional.test, &case.value),
                        default: default.as_deref() == Some(case.value.as_str()),
                    });
                    let nested = format!("{prefix}{}|", conditional.name);
                    ports(&case.inputs, &nested, &inner, formats, profile, found);
                }
            }
        }
    }
}

/// The JSON Schema that the parameters meet when every branch is chosen. A default case also
/// holds while its conditional is not given.
fn condition(branches: &[Branch]) -> Option<Json> {
    let mut all: Vec<Json> = branches
        .iter()
        .map(|branch| {
            let mut schema = json!({"properties": {&branch.test: {"const": branch.value}}});
            if !branch.default {
                schema["required"] = json!([branch.test]);
            }
            for part in branch.path.iter().rev() {
                let mut outer = json!({"properties": {part: schema}});
                if !branch.default {
                    outer["required"] = json!([part]);
                }
                schema = outer;
            }
            schema
        })
        .collect();
    match all.len() {
        0 => None,
        1 => all.pop(),
        _ => Some(json!({"allOf": all})),
    }
}

/// The JSON value of a `<when value>` for the test param: booleans for a boolean test.
fn when_value(test: &Param, value: &str) -> Json {
    match test.kind {
        ParamKind::Boolean if value == test.truevalue => Json::Bool(true),
        ParamKind::Boolean if value == test.falsevalue => Json::Bool(false),
        _ => Json::String(value.to_owned()),
    }
}

/// The `<when value>` that a raw test param value chooses.
pub fn case_value(test: &Param, value: &Json) -> Option<String> {
    match (&test.kind, value) {
        (ParamKind::Boolean, Json::Bool(true)) => Some(test.truevalue.clone()),
        (ParamKind::Boolean, _) => Some(test.falsevalue.clone()),
        (_, Json::String(text)) => Some(text.clone()),
        (_, Json::Null) => None,
        (_, other) => Some(other.to_string()),
    }
}

fn terms(datatypes: &[String], formats: &BTreeMap<String, String>) -> Vec<String> {
    let mapped: Option<Vec<String>> = datatypes
        .iter()
        .map(|datatype| formats.get(datatype).cloned())
        .collect();
    let mut mapped = mapped.unwrap_or_default();
    mapped.sort();
    mapped.dedup();
    mapped
}

/// The schema of the non-data params of a group: sections as objects, repeats as arrays, and
/// each case of a conditional as an `if` and `then`.
fn object(inputs: &[Input], profile: Option<&str>) -> Json {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for input in inputs {
        match input {
            Input::Param(param) => {
                if let Some(schema) = param_schema(param, profile) {
                    let needed = matches!(param.kind, ParamKind::Integer | ParamKind::Float)
                        && !param.optional
                        && param.initial(profile).is_null();
                    if needed {
                        required.push(Json::String(param.name.clone()));
                    }
                    properties.insert(param.name.clone(), schema);
                }
            }
            Input::Section(section) => {
                properties.insert(section.name.clone(), object(&section.inputs, profile));
            }
            Input::Repeat(repeat) => {
                let items = object(&repeat.inputs, profile);
                properties.insert(
                    repeat.name.clone(),
                    json!({"type": "array", "items": items}),
                );
            }
            Input::Conditional(conditional) => {
                let test = &conditional.test;
                let mut schema = json!({"type": "object", "properties": {}});
                if let Some(test_schema) = param_schema(test, profile) {
                    schema["properties"][&test.name] = test_schema;
                }
                let default = case_value(test, &test.initial(profile));
                let cases: Vec<Json> = (conditional.cases.iter())
                    .map(|case| {
                        let mut when = json!({"properties": {&test.name: {"const": when_value(test, &case.value)}}});
                        if default.as_deref() != Some(case.value.as_str()) {
                            when["required"] = json!([test.name]);
                        }
                        json!({"if": when, "then": object(&case.inputs, profile)})
                    })
                    .collect();
                if !cases.is_empty() {
                    schema["allOf"] = Json::Array(cases);
                }
                properties.insert(conditional.name.clone(), schema);
            }
        }
    }
    let mut schema = json!({"type": "object", "properties": properties});
    if !required.is_empty() {
        schema["required"] = Json::Array(required);
    }
    schema
}

fn param_schema(param: &Param, profile: Option<&str>) -> Option<Json> {
    let values = || -> Vec<Json> {
        (param.options.iter())
            .map(|option| Json::String(option.value.clone()))
            .collect()
    };
    let mut schema = match param.kind {
        ParamKind::Data | ParamKind::Collection => return None,
        ParamKind::Text | ParamKind::Hidden | ParamKind::Color => json!({"type": "string"}),
        ParamKind::Integer => json!({"type": "integer"}),
        ParamKind::Float => json!({"type": "number"}),
        ParamKind::Boolean => json!({"type": "boolean"}),
        ParamKind::Select if param.dynamic => json!({"type": "string"}),
        ParamKind::Select => json!({"enum": values()}),
        ParamKind::Other(_) => json!({}),
    };
    if param.kind == ParamKind::Select && param.multiple {
        schema = json!({"type": "array", "items": schema});
    }
    for (key, bound) in [("minimum", &param.min), ("maximum", &param.max)] {
        let number = bound
            .as_deref()
            .and_then(|text| text.trim().parse::<f64>().ok());
        if let Some(number) =
            number.filter(|_| matches!(param.kind, ParamKind::Integer | ParamKind::Float))
        {
            schema[key] = json!(number);
        }
    }
    if let Some(label) = &param.label {
        schema["title"] = json!(label);
    }
    if let Some(help) = &param.help {
        schema["description"] = json!(help);
    }
    match param.initial(profile) {
        Json::Null => {}
        default => schema["default"] = default,
    }
    Some(schema)
}
