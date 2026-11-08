//! WDL native blocks: the WDL driver of uwe as a child, with the op inputs as the input object
//! of the workflow, and its outputs as op outputs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::{Map, Value as Json, json};
use wdl_runtime::{Options, WdlDriver};
use wir::{Port, Type, TypeDef, resolve};
use workflow_graph::{DriverError, Launch};

use crate::child::Block;

/// A WDL driver for the launch of one block. A child never prints its outputs.
pub(crate) fn block(launch: &Launch, options: &Options) -> Result<Box<dyn Block>, DriverError> {
    let options = Options {
        print: false,
        ..options.clone()
    };
    Ok(Box::new(WdlDriver::new(launch, options)?))
}

impl Block for WdlDriver {
    /// The outputs of the workflow, which a simulated run gives too.
    fn exported(&self) -> Result<Option<Json>, DriverError> {
        Ok(WdlDriver::exported(self))
    }
}

/// The WDL input file of a native op: the inputs that are not None, and None for an input
/// whose flag `given.<input>` is set, as a call gave `None` to it.
pub(crate) fn job(mut values: Map<String, Json>) -> Json {
    let flags: Vec<String> = (values.keys())
        .filter(|name| name.starts_with("given."))
        .cloned()
        .collect();
    let mut given = Vec::new();
    for flag in flags {
        if values.remove(&flag) == Some(Json::Bool(true)) {
            given.push(flag["given.".len()..].to_owned());
        }
    }
    let mut job = crate::nextflow::params(&values);
    if let Json::Object(job) = &mut job {
        for name in given {
            job.entry(name).or_insert(Json::Null);
        }
    }
    job
}

/// The op inputs with each tuple of two as a WDL pair, an object with `left` and `right`, as
/// WDL input files give pairs.
pub(crate) fn pairs(
    bound: Map<String, Json>,
    ports: &[Port],
    defs: &[TypeDef],
) -> Map<String, Json> {
    (bound.into_iter())
        .map(|(name, value)| {
            let port = ports.iter().find(|port| port.name == name);
            let value = match port {
                Some(port) => paired(value, &port.r#type, defs),
                None => value,
            };
            (name, value)
        })
        .collect()
}

fn paired(value: Json, r#type: &Type, defs: &[TypeDef]) -> Json {
    match (resolve(r#type, defs), value) {
        (Some(Type::Optional(inner)), value) => paired(value, inner, defs),
        (Some(Type::Tuple(types)), Json::Array(items)) if types.len() == 2 && items.len() == 2 => {
            let mut sides = types
                .iter()
                .zip(items)
                .map(|(want, item)| paired(item, want, defs));
            let (left, right) = (sides.next(), sides.next());
            json!({"left": left, "right": right})
        }
        (Some(Type::Array(array)), Json::Array(items)) => Json::Array(
            (items.into_iter())
                .map(|item| paired(item, &array.items, defs))
                .collect(),
        ),
        (Some(Type::Map(map)), Json::Object(object)) => Json::Object(
            (object.into_iter())
                .map(|(key, item)| (key, paired(item, &map.value, defs)))
                .collect(),
        ),
        (Some(Type::Record(fields)), Json::Object(object)) => Json::Object(
            (object.into_iter())
                .map(
                    |(key, item)| match fields.iter().find(|field| field.name == key) {
                        Some(field) => (key, paired(item, &field.r#type, defs)),
                        None => (key, item),
                    },
                )
                .collect(),
        ),
        (_, value) => value,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::pairs;

    #[test]
    fn tuples_become_pairs() {
        let ports = json!([
            {"name": "pair", "type": {"tuple": ["int", "string"]}},
            {"name": "pairs", "type": {"optional": {"array": {"items": {"tuple": ["int", "int"]}}}}},
            {"name": "list", "type": {"array": {"items": "int"}}},
        ]);
        let ports: Vec<wir::Port> = serde_json::from_value(ports).expect("ports");
        let bound = json!({"pair": [1, "a"], "pairs": [[1, 2]], "list": [1, 2]});
        let bound = bound.as_object().cloned().unwrap_or_default();
        let expected = json!({
            "pair": {"left": 1, "right": "a"},
            "pairs": [{"left": 1, "right": 2}],
            "list": [1, 2],
        });
        assert_eq!(
            serde_json::Value::Object(pairs(bound, &ports, &[])),
            expected
        );
    }
}
