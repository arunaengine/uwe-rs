//! Select, merge and flatten ops on values: arrays made and picked in the engine.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::{Map, Value as Json};
use wir::reserved::Pick;
use wir::{Kind, Op, TypeDef};

use crate::state::widen;

/// The value of the one output of a select, merge or flatten op from its input values, in
/// the order of its input ports, or why it has none.
pub fn evaluate(op: &Op, bound: &Map<String, Json>, defs: &[TypeDef]) -> Result<Json, String> {
    let mut values: Vec<Json> = (op.inputs.iter())
        .map(|port| bound.get(&port.name).cloned().unwrap_or_default())
        .collect();
    let value = match &op.kind {
        Kind::Merge(_) => Json::Array(values),
        Kind::Flatten(_) => match values.pop() {
            Some(Json::Array(items)) => Json::Array(flat(items)),
            _ => return Err("the input is not an array".into()),
        },
        Kind::Select(select) => match values.pop() {
            Some(Json::Array(items)) => pick(select.rule, items)?,
            _ => return Err("the input is not an array".into()),
        },
        _ => return Err("not a select, merge or flatten op".into()),
    };
    let output = &op.outputs[0];
    let value = widen(value, &output.r#type, defs);
    match wir::fits(&value, &output.r#type, defs) {
        true => Ok(value),
        false => Err(format!("the value does not fit output {}", output.name)),
    }
}

/// The items of each array item, and each item that is not an array, in order.
fn flat(items: Vec<Json>) -> Vec<Json> {
    let mut flat = Vec::new();
    for item in items {
        match item {
            Json::Array(inner) => flat.extend(inner),
            other => flat.push(other),
        }
    }
    flat
}

/// The items that are set, by the rule of a select.
fn pick(rule: Pick, items: Vec<Json>) -> Result<Json, String> {
    let mut set: Vec<Json> = items.into_iter().filter(|item| !item.is_null()).collect();
    match (rule, set.len()) {
        (Pick::All, _) => Ok(Json::Array(set)),
        (Pick::First | Pick::Only, 0) => Err("no item is set".into()),
        (Pick::Only, count) if count > 1 => Err(format!("{count} items are set, not one")),
        (Pick::First | Pick::Only, _) => Ok(set.swap_remove(0)),
    }
}
