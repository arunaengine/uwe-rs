//! The record helpers of nf-bactopia on lists; `functions.nf` applies them to channels too.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::rc::Rc;

use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::value::{Set, SetKind, Value};

use super::{argument, as_record, record};
use crate::nf_schema::{entries, get};

/// The native helpers that `functions.nf` calls, by their names there.
pub(super) fn call(name: &str, arguments: &[Value]) -> Result<Value, Error> {
    match (name, arguments) {
        ("bactopiaGather", [results, mapping, meta]) => gather(results, mapping, meta),
        ("bactopiaFilter", [input, fields]) => filter(input, fields),
        ("bactopiaLogs", [results]) => {
            let mut logs = Vec::new();
            for result in items(results, "results")? {
                let meta = get(&result, "meta").unwrap_or(Value::Null);
                for log in items(&get(&result, "nf_logs").unwrap_or(Value::Null), "nf_logs")? {
                    logs.push(Value::list(vec![meta.clone(), log]));
                }
            }
            Ok(Value::list(logs))
        }
        ("bactopiaCombine", [gathered, values, field]) => {
            let field = field.to_string();
            let mut combined = Vec::new();
            for record in items(gathered, "gathered")? {
                for value in items(values, "items")? {
                    let mut fields = entries(&record);
                    fields.retain(|(key, _)| *key != field);
                    fields.push((field.clone(), value));
                    combined.push(super::record(
                        fields
                            .iter()
                            .map(|(key, value)| (key.as_str(), value.clone()))
                            .collect(),
                    ));
                }
            }
            Ok(Value::list(combined))
        }
        ("bactopiaFormat", [samples, types]) => {
            let count = match types.to_string().parse::<usize>() {
                Ok(count @ 1..=3) => count,
                _ => {
                    let message = format!("dataTypes must be 1, 2, or 3 (received: {types})");
                    return Err(argument(message));
                }
            };
            let cut = (items(samples, "samples")?.into_iter())
                .map(|sample| match &sample {
                    Value::List(list) if count < 3 => Value::list(
                        list.borrow()
                            .items
                            .iter()
                            .take(count + 1)
                            .cloned()
                            .collect(),
                    ),
                    _ => sample,
                })
                .collect();
            Ok(Value::list(cut))
        }
        _ => Err(Error::Unsupported {
            message: format!(
                "the function `{name}` of the plugin `nf-bactopia` with these arguments"
            ),
            span: Default::default(),
        }),
    }
}

/// The items of a list argument; null is an error with the argument's name.
fn items(value: &Value, name: &str) -> Result<Vec<Value>, Error> {
    match value {
        Value::Null => Err(argument(format!("{name} cannot be null"))),
        Value::List(list) => Ok(list.borrow().items.clone()),
        Value::Set(set) => Ok(set.borrow().items.iter().map(|key| key.0.clone()).collect()),
        other => Ok(vec![other.clone()]),
    }
}

/// One record of `meta` and, for each field of `mapping`, the set of its values over the
/// results under the mapped name; null when every set is empty.
fn gather(results: &Value, mapping: &Value, meta: &Value) -> Result<Value, Error> {
    let results = items(results, "results")?;
    if matches!(meta, Value::Null) {
        return Err(argument("meta cannot be null"));
    }
    if get(meta, "name").is_none_or(|name| matches!(name, Value::Null)) {
        return Err(argument("meta.name is required"));
    }
    let mapping = entries(mapping);
    if mapping.is_empty() || mapping.iter().any(|(field, _)| field.is_empty()) {
        return Err(argument("field cannot be null or empty"));
    }
    let mut fields = vec![("meta".to_owned(), as_record(meta))];
    let mut found = Vec::new();
    for (field, name) in &mapping {
        let values = results.iter().filter_map(|result| get(result, field));
        let values: Vec<Value> = values
            .filter(|value| !matches!(value, Value::Null))
            .collect();
        found.push(!values.is_empty());
        fields.push((name.to_string(), hash_set(values)));
    }
    Ok(match found.iter().any(|found| *found) {
        true => record(
            fields
                .iter()
                .map(|(key, value)| (key.as_str(), value.clone()))
                .collect(),
        ),
        false => Value::Null,
    })
}

/// The records where one of the fields has data, each as its `meta` and those fields.
fn filter(input: &Value, fields: &Value) -> Result<Value, Error> {
    let fields: Vec<String> = items(fields, "fields")?
        .iter()
        .map(Value::to_string)
        .collect();
    let has_data = |value: &Value| match value {
        Value::Null => false,
        Value::List(list) => !list.is_empty(),
        Value::Set(set) => !set.borrow().items.is_empty(),
        Value::Map(map) => !map.borrow().entries.is_empty(),
        _ => true,
    };
    let mut kept = Vec::new();
    for item in items(input, "input")? {
        let found = fields
            .iter()
            .any(|field| get(&item, field).is_some_and(|v| has_data(&v)));
        if found {
            let mut projected = vec![("meta", get(&item, "meta").unwrap_or(Value::Null))];
            for field in &fields {
                projected.push((field, get(&item, field).unwrap_or(Value::Null)));
            }
            kept.push(record(projected));
        }
    }
    Ok(Value::list(kept))
}

/// A `java.util.HashSet` of the values, in Java's iteration order.
fn hash_set(values: Vec<Value>) -> Value {
    let mut set = Set {
        kind: SetKind::HashSet(16),
        items: Default::default(),
    };
    for value in values {
        set.add(value);
    }
    Value::Set(Rc::new(RefCell::new(set)))
}
