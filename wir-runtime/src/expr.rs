//! Expr ops: a CWL expression over the input values of an op, run by the CWL evaluator of uwe.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::sync::Arc;

use cwl_parser::model::Expression;
use cwl_runtime::{Evaluator, JavaScriptSettings, Value, Variables};
use serde_json::{Map, Value as Json, json};
use wir::{Port, Type, TypeDef, resolve};

use crate::cwl::{to_cwl, to_wir};
use crate::state::widen;

/// The value of the output of an expr op from its input values, or why it has none. In a
/// simulated run, whose files are made up, code over `self` that fails gives a placeholder.
pub fn evaluate(
    spec: &wir::Expr,
    bound: &Map<String, Json>,
    output: &Port,
    defs: &[TypeDef],
    (settings, simulated): (&JavaScriptSettings, bool),
) -> Result<Json, String> {
    match compute(spec, bound, output, defs, settings) {
        Err(_) if simulated && spec.code.contains("self") => {
            Ok(placeholder(&output.r#type, &output.name, defs))
        }
        result => result,
    }
}

fn compute(
    spec: &wir::Expr,
    bound: &Map<String, Json>,
    output: &Port,
    defs: &[TypeDef],
    settings: &JavaScriptSettings,
) -> Result<Json, String> {
    let expression = Expression::parse(&spec.code, false)?;
    let inputs = value(Json::Object(bound.clone()))?;
    let this = match bound.get("self") {
        Some(item) => value(item.clone())?,
        None => Value::Null,
    };
    let runtime = Value::Map(Default::default());
    let evaluator = Evaluator {
        javascript: Some(Arc::new(spec.library.clone())),
        settings: settings.clone(),
    };
    let variables = Variables {
        inputs: &inputs,
        this: &this,
        runtime: &runtime,
    };
    let result = evaluator.evaluate(&expression, variables)?;
    let json: Json = serde_json::from_str(&result.to_json()).map_err(|error| error.to_string())?;
    let json = integral(
        widen(to_wir(json), &output.r#type, defs),
        &output.r#type,
        defs,
    );
    match wir::fits(&json, &output.r#type, defs) {
        true => Ok(json),
        false => Err(format!(
            "the value {json} does not fit output {}",
            output.name
        )),
    }
}

/// The value of the code of a task argument over input values whose Files and Directories
/// have the paths where the task stages them.
pub fn argument(
    (code, this): (&str, Option<&str>),
    staged: Json,
    (library, settings): (&[String], &JavaScriptSettings),
) -> Result<Json, String> {
    let expression = Expression::parse(code, false)?;
    let mut inputs = value(staged)?;
    listed(&mut inputs);
    let this = this.and_then(|name| inputs.get(name)).cloned();
    let evaluator = Evaluator {
        javascript: Some(Arc::new(library.to_vec())),
        settings: settings.clone(),
    };
    let variables = Variables {
        inputs: &inputs,
        this: &this.unwrap_or(Value::Null),
        runtime: &Value::Map(Default::default()),
    };
    let result = evaluator.evaluate(&expression, variables)?;
    let json: Json = serde_json::from_str(&result.to_json()).map_err(|error| error.to_string())?;
    Ok(to_wir(json))
}

/// Gives each entry of the listing of a staged Directory that has no path its path inside.
fn listed(value: &mut Value) {
    match value {
        Value::List(items) => items.iter_mut().for_each(listed),
        Value::Map(map) => {
            let path = map.get("path").and_then(Value::as_str).map(str::to_owned);
            if let (Some(path), Some(Value::List(items))) = (path, map.get_mut("listing")) {
                for item in items {
                    let Value::Map(entry) = item else { continue };
                    let name = entry.get("basename").and_then(Value::as_str);
                    if let Some(name) = name.filter(|_| !entry.contains_key("path")) {
                        let inside = format!("{path}/{name}");
                        entry.insert("dirname".into(), Value::string(&path));
                        entry.insert("path".into(), Value::String(inside));
                    }
                }
            }
            map.values_mut().for_each(listed);
        }
        _ => {}
    }
}

/// A value of a type named after `name`, like the placeholders of uwe's CWL driver: arrays of
/// two items, the first member of a union, and files that do not exist.
pub(crate) fn placeholder(r#type: &Type, name: &str, defs: &[TypeDef]) -> Json {
    match resolve(r#type, defs) {
        Some(Type::Bool) => Json::Bool(false),
        Some(Type::Int) => Json::from(1),
        Some(Type::Float) => Json::from(1.0),
        Some(Type::Optional(inner)) => placeholder(inner, name, defs),
        Some(Type::Union(members)) => match members.first() {
            Some(member) => placeholder(member, name, defs),
            None => Json::Null,
        },
        Some(Type::Enum(symbols)) => symbols.first().map_or(Json::Null, |symbol| json!(symbol)),
        Some(Type::Array(array)) => json!([
            placeholder(&array.items, name, defs),
            placeholder(&array.items, &format!("{name}_2"), defs)
        ]),
        Some(Type::Tuple(items)) => Json::Array(
            (items.iter())
                .map(|item| placeholder(item, name, defs))
                .collect(),
        ),
        Some(Type::Record(fields)) => Json::Object(
            (fields.iter())
                .map(|field| {
                    let value = placeholder(&field.r#type, &field.name, defs);
                    (field.name.clone(), value)
                })
                .collect(),
        ),
        Some(Type::Map(_)) => json!({}),
        Some(Type::File(_)) => {
            json!({"class": "File", "location": format!("_:{name}"), "basename": name})
        }
        Some(Type::Directory) => {
            json!({"class": "Directory", "location": format!("_:{name}"), "basename": name})
        }
        _ => json!(name),
    }
}

/// A wir value as CWL expressions see it: Files and Directories with `path`, `nameroot` and
/// `nameext`.
fn value(json: Json) -> Result<Value, String> {
    let mut value = Value::parse(&to_cwl(json).to_string())?;
    cwl_runtime::files::normalize(&mut value)?;
    cwl_runtime::files::visit_files(&mut value, &mut |file| {
        let location = file.get("location").and_then(Value::as_str);
        let path = location.and_then(|text| match text.starts_with('/') {
            true => Some(text.to_owned()),
            false => cwl_runtime::files::local_path(text).map(|path| path.display().to_string()),
        });
        if let Some(path) = path.filter(|_| !file.contains_key("path")) {
            file.insert("path".into(), Value::String(path));
        }
    });
    Ok(value)
}

/// A whole float where an `int` is wanted becomes an integer, as JavaScript has one kind of
/// number.
fn integral(json: Json, r#type: &Type, defs: &[TypeDef]) -> Json {
    match (resolve(r#type, defs), &json) {
        (Some(Type::Optional(inner)), _) => integral(json, inner, defs),
        (Some(Type::Int), Json::Number(number)) if number.is_f64() => {
            let float = number.as_f64().unwrap_or(f64::NAN);
            let whole = float.fract() == 0.0 && float.abs() < 9.0e18;
            match whole {
                true => Json::from(float as i64),
                false => json,
            }
        }
        _ => json,
    }
}
