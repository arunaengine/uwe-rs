//! What pandas treats as a sequence of values, and the positions that keys select.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::{Array, IndexObject, Series};
use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::value::Value;

/// Whether pandas treats a value as a sequence of values rather than as one value.
pub fn is_list_like(value: &Value) -> bool {
    match value.plain() {
        Value::List(_)
        | Value::Tuple(_)
        | Value::Set(_)
        | Value::FrozenSet(_)
        | Value::Range(_)
        | Value::Iterator(_)
        | Value::Dict(_) => true,
        Value::Native(object) => {
            object.as_any().is::<Series>()
                || object.as_any().is::<Array>()
                || object.as_any().is::<IndexObject>()
        }
        _ => false,
    }
}

/// The values of a sequence, a series, an index or an array; one text is one value.
pub fn seq_values(interpreter: &mut Interpreter, value: &Value) -> Result<Vec<Value>, Error> {
    if let Some(series) = value.as_native::<Series>() {
        return Ok(series.data().values);
    }
    if let Some(index) = value.as_native::<IndexObject>() {
        return Ok(index.labels.borrow().values.clone());
    }
    if let Some(array) = value.as_native::<Array>() {
        return Ok(array.values.borrow().clone());
    }
    match value.plain() {
        Value::Str(_) | Value::None | Value::Int(_) | Value::Float(_) | Value::Bool(_) => {
            Ok(vec![value.clone()])
        }
        _ => interpreter.iterate(value),
    }
}

/// Whether a key is a boolean mask of the given length.
pub fn is_mask(interpreter: &mut Interpreter, key: &Value, length: usize) -> Result<bool, Error> {
    if !is_list_like(key)
        || matches!(
            key.plain(),
            Value::Dict(_) | Value::Iterator(_) | Value::Tuple(_)
        )
    {
        return Ok(false);
    }
    let values = seq_values(interpreter, key)?;
    Ok(!values.is_empty()
        && values.len() == length
        && values
            .iter()
            .all(|value| matches!(value.plain(), Value::Bool(_))))
}

/// The positions a positional key selects: a position, a list of them, a slice or a mask.
pub fn positions_of(
    interpreter: &mut Interpreter,
    key: &Value,
    length: usize,
) -> Result<Vec<usize>, Error> {
    if let Value::Slice(_) = key.plain() {
        return crate::interpreter::call::slice_indexes(key, length as i64);
    }
    if is_mask(interpreter, key, length)? {
        let values = seq_values(interpreter, key)?;
        return Ok(values
            .iter()
            .enumerate()
            .filter(|(_, value)| matches!(value.plain(), Value::Bool(true)))
            .map(|(position, _)| position)
            .collect());
    }
    let keys = if is_list_like(key) {
        seq_values(interpreter, key)?
    } else {
        vec![key.clone()]
    };
    keys.iter()
        .map(|key| {
            crate::interpreter::call::normalize_index(
                key,
                length as i64,
                "positional indexers are out-of-bounds: index",
            )
        })
        .collect()
}

/// The positions of a mask, or of a slice of positions, which `[]` selects by position.
pub fn mask_or_positional_slice(
    interpreter: &mut Interpreter,
    key: &Value,
    labels: &super::labels::Labels,
) -> Result<Option<Vec<usize>>, Error> {
    if let Some(positions) = aligned_mask(key, labels)? {
        return Ok(Some(positions));
    }
    let length = labels.len();
    if is_mask(interpreter, key, length)? {
        return positions_of(interpreter, key, length).map(Some);
    }
    if let Value::Slice(parts) = key.plain()
        && parts
            .iter()
            .all(|part| matches!(part.plain(), Value::None | Value::Int(_)))
    {
        return positions_of(interpreter, key, length).map(Some);
    }
    Ok(None)
}

pub(super) fn aligned_mask(
    key: &Value,
    labels: &super::labels::Labels,
) -> Result<Option<Vec<usize>>, Error> {
    let Some(series) = key.as_native::<Series>() else {
        return Ok(None);
    };
    let data = series.data();
    if !data
        .values
        .iter()
        .all(|value| matches!(value.plain(), Value::Bool(_)))
    {
        return Ok(None);
    }
    let identical = labels.len() == data.index.len()
        && labels
            .values
            .iter()
            .zip(&data.index.values)
            .all(|(a, b)| super::same_label(a, b));
    let mut positions = Vec::new();
    for (position, label) in labels.values.iter().enumerate() {
        let source = if identical {
            position
        } else {
            let found: Vec<usize> = data
                .index
                .values
                .iter()
                .enumerate()
                .filter(|(_, known)| super::same_label(known, label))
                .map(|(position, _)| position)
                .collect();
            if found.len() != 1 {
                return Err(Error::new(
                    "IndexingError",
                    "Unalignable boolean Series provided as indexer",
                ));
            }
            found[0]
        };
        if matches!(data.values[source].plain(), Value::Bool(true)) {
            positions.push(position);
        }
    }
    Ok(Some(positions))
}
