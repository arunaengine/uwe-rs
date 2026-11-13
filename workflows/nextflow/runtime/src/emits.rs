//! Keeps the values of the channels that the entry workflow emits, and gives them as JSON.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::rc::Rc;

use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::number::Number;
use nextflow_evaluator::value::Value;
use serde_json::{Map, Value as Json, json};

use crate::channel::Message;
use crate::network::{Behaviour, Context, Network, State};

/// One emit of the entry workflow, with the values its channel gave so far.
pub(crate) struct Emit {
    name: Rc<str>,
    /// Whether the channel is a value channel, which gives one value.
    single: bool,
    values: Rc<RefCell<Vec<Value>>>,
}

/// Reads every value of an emitted channel until it closes.
struct Record(Rc<RefCell<Vec<Value>>>);

impl Behaviour for Record {
    fn run(&mut self, context: &mut Context) -> Result<State, Error> {
        while let Some(message) = context.read(0) {
            match message {
                Message::Value(value) => self.0.borrow_mut().push(value),
                Message::Stop => return Ok(State::Stopped),
            }
        }
        Ok(State::Running)
    }
}

/// Adds a reader to each channel in the outputs of the entry workflow, before the network
/// starts, so that it sees every value.
pub(crate) fn record(network: &mut Network, emitted: &Value) -> Vec<Emit> {
    let Value::ChannelOut(out) = emitted else {
        return Vec::new();
    };
    let mut emits = Vec::new();
    for (value, name) in out.values.iter().zip(&out.names) {
        let (Value::Channel(channel), Some(name)) = (value, name) else {
            continue;
        };
        let values = Rc::new(RefCell::new(Vec::new()));
        let reader = Box::new(Record(values.clone()));
        network.add(&format!("emit {name}"), &[*channel], reader);
        emits.push(Emit {
            name: name.clone(),
            single: network.is_value(*channel),
            values,
        });
    }
    emits
}

/// The emits by name: a value channel gives its value, or null when it closed empty, and a
/// queue channel the array of its values in the order they came.
pub(crate) fn to_json(emits: &[Emit]) -> Result<Json, String> {
    let mut object = Map::new();
    for emit in emits {
        let values = emit.values.borrow();
        let items = (values.iter()).map(|value| convert(value).map_err(|kind| (emit, kind)));
        let items: Result<Vec<Json>, _> = items.collect();
        let items = items.map_err(|(emit, kind)| {
            format!(
                "the emit {} holds {kind}, which has no JSON form",
                emit.name
            )
        })?;
        let value = match emit.single {
            true => items.into_iter().next().unwrap_or(Json::Null),
            false => Json::Array(items),
        };
        object.insert(emit.name.to_string(), value);
    }
    Ok(Json::Object(object))
}

/// A value as JSON: a path becomes a File object with its location and name; what JSON
/// cannot hold, like a closure, gives its kind.
fn convert(value: &Value) -> Result<Json, &'static str> {
    Ok(match value {
        Value::Null => Json::Null,
        Value::Boolean(value) => Json::Bool(*value),
        Value::Number(Number::Integer(value)) => json!(value),
        Value::Number(Number::Long(value)) => json!(value),
        Value::Number(number) => {
            let number = serde_json::Number::from_f64(number.to_f64());
            Json::Number(number.ok_or("a number that is not finite")?)
        }
        Value::String(text) | Value::GString(text) => json!(text.as_ref()),
        Value::Character(_) | Value::MemoryUnit(_) | Value::Duration(_) => {
            json!(value.to_string())
        }
        Value::List(list) => Json::Array(
            list.borrow()
                .items
                .iter()
                .map(convert)
                .collect::<Result<_, _>>()?,
        ),
        Value::Set(set) => Json::Array(
            (set.borrow().items.iter())
                .map(|key| convert(&key.0))
                .collect::<Result<_, _>>()?,
        ),
        Value::Map(map) => {
            let map = map.borrow();
            let entries = map.entries.iter().map(|(key, item)| {
                let key = match &key.0 {
                    Value::String(text) | Value::GString(text) => text.to_string(),
                    other => other.to_string(),
                };
                Ok::<_, &'static str>((key, convert(item)?))
            });
            Json::Object(entries.collect::<Result<_, _>>()?)
        }
        Value::Path(path) => {
            let location = match path.prefix {
                None => path.path.clone(),
                Some(_) => path.uri(),
            };
            json!({"class": "File", "location": location, "basename": path.name()})
        }
        other => return Err(other.class_name()),
    })
}
