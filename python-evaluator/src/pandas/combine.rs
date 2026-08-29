//! `pandas.concat` and `pandas.merge`: tables and series put together.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::frame::{DataFrame, FrameData};
use super::labels::Labels;
use super::series::{Series, SeriesData};
use super::{flag, nan, same_label, seq_values};
use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::value::{Args, Value, key_equal};

/// A table or series as a table; a series is one column named after it.
fn as_frame(value: &Value) -> Option<FrameData> {
    if let Some(frame) = value.as_native::<DataFrame>() {
        return Some(frame.data());
    }
    let series = value.as_native::<Series>()?.data();
    let name = if series.name.is_none() {
        Value::Int(0)
    } else {
        series.name.clone()
    };
    Some(FrameData::new(
        Labels::single(vec![name], Value::None),
        vec![series.values],
        series.index,
    ))
}

/// `pd.concat(objs, axis=0, ignore_index=False)`.
pub fn concat(interpreter: &mut Interpreter, mut args: Args) -> Result<Value, Error> {
    let objects = args
        .take(0, "objs")
        .ok_or_else(|| Error::type_error("concat() missing objs"))?;
    let axis = args.take(1, "axis").unwrap_or(Value::Int(0));
    let ignore_index = flag(interpreter, &mut args, "ignore_index", false)?;
    let objects: Vec<Value> = match objects.plain() {
        Value::Dict(_) => interpreter
            .mapping_items(&objects)?
            .into_iter()
            .map(|(_, value)| value)
            .collect(),
        _ => interpreter.iterate(&objects)?,
    };
    let objects: Vec<Value> = objects
        .into_iter()
        .filter(|object| !object.is_none())
        .collect();
    if objects.is_empty() {
        return Err(Error::value_error("No objects to concatenate"));
    }
    let by_columns = matches!(axis.plain(), Value::Int(1)) || axis.as_str() == Some("columns");
    if !by_columns
        && objects
            .iter()
            .all(|object| object.as_native::<Series>().is_some())
    {
        let parts: Vec<SeriesData> = objects
            .iter()
            .filter_map(|object| object.as_native::<Series>())
            .map(Series::data)
            .collect();
        let name = parts[0].name.clone();
        let name = if parts.iter().all(|part| same_label(&part.name, &name)) {
            name
        } else {
            Value::None
        };
        let values: Vec<Value> = parts
            .iter()
            .flat_map(|part| part.values.iter().cloned())
            .collect();
        let index = if ignore_index {
            Labels::range(values.len())
        } else {
            Labels {
                values: parts
                    .iter()
                    .flat_map(|part| part.index.values.iter().cloned())
                    .collect(),
                names: parts[0].index.names.clone(),
            }
        };
        return Ok(Series::value(values, index, name));
    }
    let mut frames = Vec::with_capacity(objects.len());
    for object in &objects {
        frames.push(as_frame(object).ok_or_else(|| {
            Error::type_error(format!(
                "cannot concatenate object of type '{}'; only Series and DataFrame objs are valid",
                crate::types::type_name(object)
            ))
        })?);
    }
    if by_columns {
        return Ok(DataFrame::from_data(side_by_side(&frames)?));
    }
    let mut columns: Vec<Value> = Vec::new();
    for frame in &frames {
        for label in &frame.columns.values {
            if !columns.iter().any(|known| key_equal(known, label)) {
                columns.push(label.clone());
            }
        }
    }
    let mut data: Vec<Vec<Value>> = vec![Vec::new(); columns.len()];
    let mut index = Vec::new();
    for frame in &frames {
        let rows = frame.rows();
        for (position, label) in columns.iter().enumerate() {
            match frame.column_position(label) {
                Some(column) => data[position].extend(frame.data[column].iter().cloned()),
                None => data[position].extend(std::iter::repeat_n(nan(), rows)),
            }
        }
        index.extend(frame.index().values);
    }
    let names = frames[0].index().names;
    let index = if ignore_index {
        Labels::range(index.len())
    } else {
        Labels {
            values: index,
            names,
        }
    };
    Ok(DataFrame::from_data(FrameData::new(
        Labels::single(columns, Value::None),
        data,
        index,
    )))
}

/// Tables side by side, their rows matched by label.
fn side_by_side(frames: &[FrameData]) -> Result<FrameData, Error> {
    let first = frames[0].index();
    if frames.iter().all(|frame| {
        let index = frame.index();
        index.len() == first.len()
            && index
                .values
                .iter()
                .zip(&first.values)
                .all(|(a, b)| same_label(a, b))
    }) {
        return Ok(FrameData::new(
            Labels::single(
                frames
                    .iter()
                    .flat_map(|frame| frame.columns.values.clone())
                    .collect(),
                Value::None,
            ),
            frames.iter().flat_map(|frame| frame.data.clone()).collect(),
            first,
        ));
    }
    if frames.iter().any(|frame| {
        let index = frame.index();
        index.values.iter().enumerate().any(|(position, label)| {
            index.values[..position]
                .iter()
                .any(|known| same_label(known, label))
        })
    }) {
        return Err(Error::new(
            "InvalidIndexError",
            "Reindexing only valid with uniquely valued Index objects",
        ));
    }
    let mut labels: Vec<Value> = Vec::new();
    for frame in frames {
        for label in frame.index().values {
            if !labels.iter().any(|known| same_label(known, &label)) {
                labels.push(label);
            }
        }
    }
    let mut columns = Vec::new();
    let mut data = Vec::new();
    for frame in frames {
        let index = frame.index();
        for (position, label) in frame.columns.values.iter().enumerate() {
            columns.push(label.clone());
            data.push(
                labels
                    .iter()
                    .map(|row| {
                        index
                            .find(row)
                            .first()
                            .map_or_else(nan, |found| frame.data[position][*found].clone())
                    })
                    .collect(),
            );
        }
    }
    Ok(FrameData::new(
        Labels::single(columns, Value::None),
        data,
        Labels::single(labels, frames[0].index().names[0].clone()),
    ))
}

fn column_names(
    interpreter: &mut Interpreter,
    value: Option<Value>,
) -> Result<Option<Vec<Value>>, Error> {
    match value {
        Some(value) if !value.is_none() => Ok(Some(if super::is_list_like(&value) {
            seq_values(interpreter, &value)?
        } else {
            vec![value]
        })),
        _ => Ok(None),
    }
}

/// `merge(left, right, how="inner", on=None, left_on=None, right_on=None, suffixes=("_x", "_y"))`.
pub fn merge(
    interpreter: &mut Interpreter,
    left: Value,
    right: Value,
    mut args: Args,
) -> Result<Value, Error> {
    let left = as_frame(&left)
        .ok_or_else(|| Error::type_error("Can only merge Series or DataFrame objects"))?;
    let right = as_frame(&right)
        .ok_or_else(|| Error::type_error("Can only merge Series or DataFrame objects"))?;
    let how = match args.take_keyword("how") {
        Some(value) => interpreter.to_str(&value)?,
        None => "inner".into(),
    };
    let on = column_names(interpreter, args.take_keyword("on"))?;
    let left_on = column_names(interpreter, args.take_keyword("left_on"))?;
    let right_on = column_names(interpreter, args.take_keyword("right_on"))?;
    if flag(interpreter, &mut args, "left_index", false)?
        || flag(interpreter, &mut args, "right_index", false)?
    {
        return Err(Error::unsupported("merge() on the index"));
    }
    let suffixes = match args.take_keyword("suffixes") {
        Some(value) => {
            let parts = interpreter.iterate(&value)?;
            let mut texts = Vec::new();
            for part in parts {
                texts.push(if part.is_none() {
                    String::new()
                } else {
                    interpreter.to_str(&part)?
                });
            }
            (
                texts.first().cloned().unwrap_or_default(),
                texts.get(1).cloned().unwrap_or_default(),
            )
        }
        None => ("_x".to_owned(), "_y".to_owned()),
    };
    let (left_keys, right_keys, shared) = match (on, left_on, right_on) {
        (Some(on), _, _) => (on.clone(), on, true),
        (None, Some(left_on), Some(right_on)) => (left_on, right_on, false),
        _ => {
            let common: Vec<Value> = left
                .columns
                .values
                .iter()
                .filter(|label| right.column_position(label).is_some())
                .cloned()
                .collect();
            if common.is_empty() {
                return Err(Error::new(
                    "MergeError",
                    "No common columns to perform merge on.",
                ));
            }
            (common.clone(), common, true)
        }
    };
    let position = |frame: &FrameData, label: &Value| {
        frame
            .column_position(label)
            .ok_or_else(|| Error::key_error(label.clone()))
    };
    let left_columns: Vec<usize> = left_keys
        .iter()
        .map(|label| position(&left, label))
        .collect::<Result<_, _>>()?;
    let right_columns: Vec<usize> = right_keys
        .iter()
        .map(|label| position(&right, label))
        .collect::<Result<_, _>>()?;
    let key_of = |frame: &FrameData, columns: &[usize], row: usize| -> Vec<Value> {
        columns
            .iter()
            .map(|column| frame.data[*column][row].clone())
            .collect()
    };
    let matches = |a: &[Value], b: &[Value]| a.iter().zip(b).all(|(a, b)| same_label(a, b));
    // The pairs of rows, `None` for a side without a match.
    let mut pairs: Vec<(Option<usize>, Option<usize>)> = Vec::new();
    let mut matched_right = vec![false; right.rows()];
    for row in 0..left.rows() {
        let key = key_of(&left, &left_columns, row);
        let found: Vec<usize> = (0..right.rows())
            .filter(|other| matches(&key, &key_of(&right, &right_columns, *other)))
            .collect();
        if found.is_empty() {
            if how == "left" || how == "outer" {
                pairs.push((Some(row), None));
            }
            continue;
        }
        for other in found {
            matched_right[other] = true;
            pairs.push((Some(row), Some(other)));
        }
    }
    if how == "right" {
        pairs.clear();
        for other in 0..right.rows() {
            let key = key_of(&right, &right_columns, other);
            let found: Vec<usize> = (0..left.rows())
                .filter(|row| matches(&key, &key_of(&left, &left_columns, *row)))
                .collect();
            if found.is_empty() {
                pairs.push((None, Some(other)));
            }
            for row in found {
                pairs.push((Some(row), Some(other)));
            }
        }
    } else if how == "outer" {
        pairs.extend(
            (0..right.rows())
                .filter(|other| !matched_right[*other])
                .map(|other| (None, Some(other))),
        );
    } else if how != "inner" && how != "left" {
        return Err(Error::unsupported(format!("merge(how={how:?})")));
    }
    let mut columns: Vec<Value> = Vec::new();
    let mut data: Vec<Vec<Value>> = Vec::new();
    let overlaps = |label: &Value, frame: &FrameData, keys: &[usize]| {
        frame
            .column_position(label)
            .is_some_and(|position| !(shared && keys.contains(&position)))
    };
    for (column, label) in left.columns.values.iter().enumerate() {
        let is_key = shared && left_columns.contains(&column);
        let label = if !is_key && overlaps(label, &right, &right_columns) {
            suffixed(interpreter, label, &suffixes.0)?
        } else {
            label.clone()
        };
        let key_position = left_columns.iter().position(|key| *key == column);
        columns.push(label);
        data.push(
            pairs
                .iter()
                .map(|(row, other)| match (row, other, key_position) {
                    (Some(row), _, _) => left.data[column][*row].clone(),
                    (None, Some(other), Some(key)) if shared => {
                        right.data[right_columns[key]][*other].clone()
                    }
                    _ => nan(),
                })
                .collect(),
        );
    }
    for (column, label) in right.columns.values.iter().enumerate() {
        if shared && right_columns.contains(&column) {
            continue;
        }
        let label = if overlaps(label, &left, &left_columns) {
            suffixed(interpreter, label, &suffixes.1)?
        } else {
            label.clone()
        };
        columns.push(label);
        data.push(
            pairs
                .iter()
                .map(|(_, other)| other.map_or_else(nan, |other| right.data[column][other].clone()))
                .collect(),
        );
    }
    let rows = pairs.len();
    Ok(DataFrame::from_data(FrameData::new(
        Labels::single(columns, Value::None),
        data,
        Labels::range(rows),
    )))
}

fn suffixed(interpreter: &mut Interpreter, label: &Value, suffix: &str) -> Result<Value, Error> {
    Ok(Value::str(format!(
        "{}{suffix}",
        interpreter.to_str(label)?
    )))
}
