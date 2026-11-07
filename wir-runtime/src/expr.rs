//! Expr ops: a CWL expression over the input values of an op, run by the CWL evaluator of uwe.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::sync::Arc;

use cwl_parser::model::Expression;
use cwl_runtime::{Evaluator, JavaScriptSettings, Value, Variables};
use serde_json::{Map, Value as Json};
use wir::{Port, Type, TypeDef, resolve};

use crate::cwl::{to_cwl, to_wir};
use crate::state::widen;

/// The value of the output of an expr op from its input values, or why it has none.
pub fn evaluate(
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
        if let Some(path) = path {
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
