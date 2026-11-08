//! The stream ops that map items one by one: transform, filter, flat_map and flatten, and
//! the value forms of transform and filter.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::Value as Json;
use wir::reserved::{Depth, NullRule};
use wir::{Body, ExprLanguage};

use crate::callback::Callbacks;
use crate::engine::Context;
use crate::operator::{CHUNK, Operator, Pull, Step, Turn};
use crate::streams::Failure;

/// What a mapping op does with each item: the items it gives for it.
pub enum Rule {
    Transform(Body, NullRule),
    Filter(Body),
    FlatMap(Option<Body>),
    Flatten(Depth, bool),
}

/// Maps the items of `source` to `out`; with a value `source`, the value once.
pub struct Mapping {
    pub rule: Rule,
    pub stream: bool,
    pub callbacks: Callbacks,
}

impl Mapping {
    pub fn new(rule: Rule, stream: bool) -> Mapping {
        Mapping {
            rule,
            stream,
            callbacks: Callbacks::default(),
        }
    }

    /// The items that one source item gives.
    fn give(
        &mut self,
        turn: &Turn<'_>,
        context: &Context<'_>,
        item: Json,
    ) -> Result<Vec<Json>, Failure> {
        let mut call = |body: &Body, item: &Json| {
            let called = self.callbacks.call(
                context,
                body,
                (&["item"], std::slice::from_ref(item)),
                &turn.values,
            );
            called.map_err(|why| turn.failed(why))
        };
        Ok(match &self.rule {
            Rule::Transform(body, nulls) => match call(body, &item)? {
                Json::Null if *nulls == NullRule::Drop => Vec::new(),
                value => vec![value],
            },
            Rule::Filter(body) => {
                let kept = call(body, &item)?;
                match truth(body, &kept) {
                    Some(true) => vec![item],
                    Some(false) => Vec::new(),
                    None => return Err(turn.failed(format!("the condition gave {kept}, no bool"))),
                }
            }
            Rule::FlatMap(body) => {
                let value = match body {
                    Some(body) => call(body, &item)?,
                    None => item,
                };
                expand(value)
            }
            Rule::Flatten(_, true) if item.is_null() => Vec::new(),
            Rule::Flatten(Depth::One, _) => match item {
                Json::Array(items) => items,
                other => vec![other],
            },
            Rule::Flatten(Depth::All, _) => {
                let mut items = Vec::new();
                deep(item, &mut items);
                items
            }
        })
    }
}

impl Operator for Mapping {
    fn step(&mut self, turn: &mut Turn<'_>, context: &Context<'_>) -> Result<Step, Failure> {
        if !self.stream {
            // A value form gives one value, or closes empty.
            let source = turn.values.get("source").cloned().unwrap_or_default();
            match self.give(turn, context, source)?.pop() {
                Some(value) => turn.bind("out", value),
                None => turn.close_empty("out"),
            }
            return Ok(Step::Done);
        }
        let mut moved = false;
        for _ in 0..CHUNK {
            match turn.pull("source")? {
                Pull::Item(item) => {
                    for value in self.give(turn, context, item)? {
                        turn.emit("out", value);
                    }
                    moved = true;
                }
                Pull::Pending => break,
                Pull::Closed => return Ok(Step::Done),
            }
        }
        Ok(if moved { Step::Moved } else { Step::Idle })
    }
}

/// The items of one collection level: the items of an array or a set, the entries of a map,
/// the key and the value of a map entry; null gives none, and any other value itself.
fn expand(value: Json) -> Vec<Json> {
    match value {
        Json::Null => Vec::new(),
        Json::Array(items) => items,
        Json::Object(object) => match object.get("class").and_then(Json::as_str) {
            Some("Set") => object.get("items").and_then(Json::as_array).cloned().unwrap_or_default(),
            Some("MapEntry") => vec![object["key"].clone(), object["value"].clone()],
            Some(_) => vec![Json::Object(object)],
            None => (object.into_iter())
                .map(|(key, value)| serde_json::json!({"class": "MapEntry", "key": key, "value": value}))
                .collect(),
        },
        other => vec![other],
    }
}

/// The items of every level of arrays and sets, in order; maps stay whole.
fn deep(value: Json, items: &mut Vec<Json>) {
    match value {
        Json::Array(inner) => inner.into_iter().for_each(|item| deep(item, items)),
        Json::Object(object) if object.get("class").and_then(Json::as_str) == Some("Set") => {
            let inner = object
                .get("items")
                .and_then(Json::as_array)
                .cloned()
                .unwrap_or_default();
            inner.into_iter().for_each(|item| deep(item, items));
        }
        other => items.push(other),
    }
}

/// Whether a value is true by the rules of the language of its body: Groovy and Python
/// truth, JavaScript truth, or a bool for graph bodies and WDL. `None` for no bool.
pub fn truth(body: &Body, value: &Json) -> Option<bool> {
    let language = match body {
        Body::Graph(_) => None,
        Body::Closure(closure) => Some(closure.language),
    };
    let empty = |value: &Json| match value {
        Json::Null | Json::Bool(false) => true,
        Json::Number(number) => number.as_f64() == Some(0.0),
        Json::String(text) => text.is_empty(),
        _ => false,
    };
    match (language, value) {
        (_, Json::Bool(flag)) => Some(*flag),
        (Some(ExprLanguage::Groovy | ExprLanguage::Python), Json::Array(items)) => {
            Some(!items.is_empty())
        }
        (Some(ExprLanguage::Groovy | ExprLanguage::Python), Json::Object(object)) => {
            Some(!object.is_empty())
        }
        (Some(ExprLanguage::Groovy | ExprLanguage::Python | ExprLanguage::Javascript), value) => {
            Some(!empty(value))
        }
        _ => None,
    }
}
