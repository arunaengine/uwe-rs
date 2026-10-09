//! Evaluates the ShmSize extension and parses Docker shared-memory size units.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use cwl_parser::model::{Expression, RequirementClass, Version};

use crate::expression::{Evaluator, Variables};
use crate::scope::Scope;
use crate::tool::{Options, unsupported};
use crate::value::Value;

pub(crate) fn size(
    scope: &Scope,
    evaluator: &Evaluator,
    vars: Variables<'_>,
    options: &Options,
    version: Version,
) -> Result<Option<i64>, String> {
    let Some((value, required)) = scope.find(|class| match class {
        RequirementClass::Other { class, value }
            if class.rsplit(['#', ':']).next() == Some("ShmSize") =>
        {
            Some(value)
        }
        _ => None,
    }) else {
        return Ok(None);
    };
    if !options.simulated && options.local_engine.is_none() {
        return if required {
            Err(unsupported(
                "ShmSize needs a local executor that sets the container shm limit",
            ))
        } else {
            Ok(None)
        };
    }
    let parsed = || -> Result<i64, String> {
        let text = match value.get("shmSize") {
            None => "64m",
            Some(value) => value.as_str().ok_or("ShmSize shmSize must be a string")?,
        };
        let expression = Expression::parse(text, version.old_escapes())?;
        let Value::String(text) = evaluator.evaluate(&expression, vars)? else {
            return Err("ShmSize shmSize must evaluate to a string".into());
        };
        size_bytes(&text)
    };
    match parsed() {
        Ok(size) => Ok(Some(size)),
        Err(_) if !required => Ok(None),
        Err(error) => Err(error),
    }
}

fn size_bytes(text: &str) -> Result<i64, String> {
    let failed = || "ShmSize needs a positive size in bytes, k, m or g".to_owned();
    let text = text.trim();
    let (number, unit) = match text.as_bytes().last() {
        Some(unit) if unit.is_ascii_alphabetic() => text.split_at(text.len() - 1),
        _ => (text, ""),
    };
    let factor = match unit.to_ascii_lowercase().as_str() {
        "" | "b" => 1.0,
        "k" => 1024.0,
        "m" => 1024.0 * 1024.0,
        "g" => 1024.0 * 1024.0 * 1024.0,
        _ => return Err(failed()),
    };
    let bytes = number.parse::<f64>().map_err(|_| failed())? * factor;
    if !bytes.is_finite() || bytes < 1.0 || bytes >= i64::MAX as f64 {
        return Err(failed());
    }
    Ok(bytes as i64)
}
