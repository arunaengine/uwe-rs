//! The methods and attributes of `pandas.Series`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::array::Array;
use super::frame::{DataFrame, FrameData};
use super::index::IndexObject;
use super::indexer::{Indexer, select};
use super::labels::Labels;
use super::series::{Series, SeriesData};
use super::{is_missing, method, nan, same_label, seq_values};
use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::value::{Args, Dict, Key, Value, key_equal};

fn series_method(
    this: &Value,
    name: &str,
    function: fn(&mut Interpreter, SeriesData, Args) -> Result<Value, Error>,
) -> Value {
    method::<Series>(this, name, move |interpreter, series, args| {
        function(interpreter, series.data(), args)
    })
}

/// A new series of booleans, one per value.
fn flags(data: &SeriesData, test: impl Fn(&Value) -> bool) -> Value {
    Series::from_data(
        data.with_values(
            data.values
                .iter()
                .map(|value| Value::Bool(test(value)))
                .collect(),
        ),
    )
}

fn head_or_tail(data: SeriesData, args: &Args, head: bool) -> Value {
    let count = args.get(0).and_then(Value::as_int).unwrap_or(5).max(0) as usize;
    let length = data.values.len();
    let positions: Vec<usize> = if head {
        (0..count.min(length)).collect()
    } else {
        (length.saturating_sub(count)..length).collect()
    };
    Series::from_data(data.take(&positions))
}

/// `Series.value_counts()`: how often each value occurs, the most frequent first.
fn value_counts(_: &mut Interpreter, data: SeriesData, _: Args) -> Result<Value, Error> {
    let mut values: Vec<Value> = Vec::new();
    let mut counts: Vec<i64> = Vec::new();
    for value in data.values.iter().filter(|value| !is_missing(value)) {
        match values.iter().position(|seen| key_equal(seen, value)) {
            Some(position) => counts[position] += 1,
            None => {
                values.push(value.clone());
                counts.push(1);
            }
        }
    }
    let mut order: Vec<usize> = (0..values.len()).collect();
    order.sort_by(|a, b| counts[*b].cmp(&counts[*a]));
    Ok(Series::value(
        order
            .iter()
            .map(|position| Value::Int(counts[*position]))
            .collect(),
        Labels::single(
            order
                .iter()
                .map(|position| values[*position].clone())
                .collect(),
            data.name,
        ),
        Value::str("count"),
    ))
}

/// `Series.reset_index(drop=False)`: the labels become columns of a table, or are dropped.
fn reset_index(
    interpreter: &mut Interpreter,
    data: SeriesData,
    mut args: Args,
) -> Result<Value, Error> {
    let drop = match args.take_keyword("drop") {
        Some(value) => interpreter.truthy(&value)?,
        None => false,
    };
    let length = data.values.len();
    if drop {
        return Ok(Series::value(data.values, Labels::range(length), data.name));
    }
    let mut columns = Vec::new();
    let mut values = Vec::new();
    for level in 0..data.index.levels() {
        let name = data.index.names[level].clone();
        columns.push(if name.is_none() {
            Value::str(if data.index.is_multi() {
                format!("level_{level}")
            } else {
                "index".into()
            })
        } else {
            name
        });
        values.push(data.index.level(level));
    }
    columns.push(if data.name.is_none() {
        Value::Int(0)
    } else {
        data.name
    });
    values.push(data.values);
    Ok(DataFrame::from_data(FrameData::new(
        Labels::single(columns, Value::None),
        values,
        Labels::range(length),
    )))
}

/// An attribute of a series: a property, a method, or the value of an index label.
pub fn attribute(this: &Value, data: SeriesData, name: &str) -> Option<Value> {
    Some(match name {
        "index" => IndexObject::value(data.index),
        "name" => data.name,
        "values" | "array" => Array::value(data.values),
        "dtype" | "dtypes" => Value::str(super::dtype_name(&data.values)),
        "shape" => Value::tuple(vec![Value::Int(data.values.len() as i64)]),
        "size" => Value::Int(data.values.len() as i64),
        "ndim" => Value::Int(1),
        "empty" => Value::Bool(data.values.is_empty()),
        "hasnans" => Value::Bool(data.values.iter().any(is_missing)),
        "is_unique" => Value::Bool(
            super::duplicated(&data.values)
                .iter()
                .all(|flag| matches!(flag, Value::Bool(false))),
        ),
        "str" => Value::native(super::strings::StrAccessor(this.clone())),
        "loc" | "iloc" | "at" | "iat" => Indexer::value(this, name),
        "tolist" | "to_list" => {
            series_method(this, name, |_, data, _| Ok(Value::list(data.values)))
        }
        "to_numpy" => series_method(this, name, |_, data, _| Ok(Array::value(data.values))),
        "unique" => series_method(this, name, |_, data, _| {
            Ok(Array::value(super::distinct(&data.values)))
        }),
        "nunique" => series_method(this, name, |_, data, _| {
            let values: Vec<Value> = data
                .values
                .into_iter()
                .filter(|value| !is_missing(value))
                .collect();
            Ok(Value::Int(super::distinct(&values).len() as i64))
        }),
        "isin" => series_method(this, name, |interpreter, data, args| {
            let wanted = seq_values(interpreter, args.get(0).unwrap_or(&Value::None))?;
            Ok(flags(&data, |value| {
                wanted.iter().any(|item| same_label(item, value))
            }))
        }),
        "isna" | "isnull" => series_method(this, name, |_, data, _| Ok(flags(&data, is_missing))),
        "notna" | "notnull" => series_method(this, name, |_, data, _| {
            Ok(flags(&data, |value| !is_missing(value)))
        }),
        "dropna" => series_method(this, name, |_, data, _| {
            let keep: Vec<usize> = (0..data.values.len())
                .filter(|position| !is_missing(&data.values[*position]))
                .collect();
            Ok(Series::from_data(data.take(&keep)))
        }),
        "fillna" => series_method(this, name, |_, data, mut args| {
            let fill = args.take(0, "value").unwrap_or(Value::None);
            let values = data
                .values
                .iter()
                .map(|value| {
                    if is_missing(value) {
                        fill.clone()
                    } else {
                        value.clone()
                    }
                })
                .collect();
            Ok(Series::from_data(data.with_values(values)))
        }),
        "astype" => series_method(this, name, |interpreter, data, mut args| {
            let kind = args.take(0, "dtype").unwrap_or(Value::None);
            let values = super::cast_values(interpreter, &data.values, &kind)?;
            Ok(Series::from_data(data.with_values(values)))
        }),
        "apply" | "map" => series_method(this, name, |interpreter, data, args| {
            let function = args.get(0).cloned().unwrap_or(Value::None);
            let values = super::map_values(interpreter, &data.values, &function)?;
            Ok(Series::from_data(data.with_values(values)))
        }),
        "replace" => series_method(this, name, |interpreter, data, args| {
            let values = super::replace_values(interpreter, &data.values, &args)?;
            Ok(Series::from_data(data.with_values(values)))
        }),
        "explode" => series_method(this, name, |interpreter, data, _| {
            // One row per item of a list; an empty list gives one missing value.
            let mut values = Vec::new();
            let mut positions = Vec::new();
            for (position, value) in data.values.iter().enumerate() {
                let items = match value.plain() {
                    Value::List(_) | Value::Tuple(_) => interpreter.iterate(value)?,
                    _ => vec![value.clone()],
                };
                if items.is_empty() {
                    values.push(nan());
                    positions.push(position);
                }
                for item in items {
                    values.push(item);
                    positions.push(position);
                }
            }
            let mut exploded = data.take(&positions);
            exploded.values = values;
            Ok(Series::from_data(exploded))
        }),
        "value_counts" => series_method(this, name, value_counts),
        "drop_duplicates" => series_method(this, name, |_, data, _| {
            let flags = super::duplicated(&data.values);
            let keep: Vec<usize> = (0..data.values.len())
                .filter(|position| matches!(flags[*position], Value::Bool(false)))
                .collect();
            Ok(Series::from_data(data.take(&keep)))
        }),
        "duplicated" => series_method(this, name, |_, data, _| {
            let flags = super::duplicated(&data.values);
            Ok(Series::from_data(data.with_values(flags)))
        }),
        "sort_values" | "sort_index" => {
            let by_index = name == "sort_index";
            method::<Series>(this, name, move |interpreter, series, mut args| {
                let data = series.data();
                let ascending = match args.take_keyword("ascending") {
                    Some(value) => interpreter.truthy(&value)?,
                    None => true,
                };
                let keys = if by_index {
                    data.index.values.clone()
                } else {
                    data.values.clone()
                };
                let order = super::sorted_positions(interpreter, &keys, !ascending)?;
                Ok(Series::from_data(data.take(&order)))
            })
        }
        "any" => series_method(this, name, |interpreter, data, _| {
            for value in data.values.iter().filter(|value| !is_missing(value)) {
                if interpreter.truthy(value)? {
                    return Ok(Value::Bool(true));
                }
            }
            Ok(Value::Bool(false))
        }),
        "all" => series_method(this, name, |interpreter, data, _| {
            for value in data.values.iter().filter(|value| !is_missing(value)) {
                if !interpreter.truthy(value)? {
                    return Ok(Value::Bool(false));
                }
            }
            Ok(Value::Bool(true))
        }),
        "sum" => series_method(this, name, |interpreter, data, _| {
            super::sum(interpreter, &data.values)
        }),
        "mean" => series_method(this, name, |interpreter, data, _| {
            let values: Vec<Value> = data
                .values
                .into_iter()
                .filter(|value| !is_missing(value))
                .collect();
            if values.is_empty() {
                return Ok(nan());
            }
            let total = super::sum(interpreter, &values)?;
            let total = crate::number::to_f64(&total).unwrap_or(f64::NAN);
            Ok(Value::Float(total / values.len() as f64))
        }),
        "min" | "max" => {
            let largest = name == "max";
            method::<Series>(this, name, move |interpreter, series, _| {
                super::extreme(interpreter, &series.data().values, largest)
            })
        }
        "count" => series_method(this, name, |_, data, _| {
            Ok(Value::Int(
                data.values
                    .iter()
                    .filter(|value| !is_missing(value))
                    .count() as i64,
            ))
        }),
        "item" | "squeeze" => {
            series_method(this, name, |_, data, _| match data.values.as_slice() {
                [single] => Ok(single.clone()),
                _ => Err(Error::value_error(
                    "can only convert an array of size 1 to a Python scalar",
                )),
            })
        }
        "get" => series_method(this, name, |interpreter, data, mut args| {
            let key = args.take(0, "key").unwrap_or(Value::None);
            let default = args.take(1, "default").unwrap_or(Value::None);
            match select(interpreter, &data.index, &key) {
                Ok(selection) if selection.scalar => {
                    Ok(data.values[selection.positions[0]].clone())
                }
                Ok(selection) => {
                    let mut taken = data.take(&selection.positions);
                    taken.index = selection.labels;
                    Ok(Series::from_data(taken))
                }
                Err(error) if error.is("KeyError") => Ok(default),
                Err(error) => Err(error),
            }
        }),
        "items" | "iteritems" => series_method(this, name, |_, data, _| {
            Ok(Value::iterator(
                data.index
                    .values
                    .iter()
                    .zip(data.values.iter())
                    .map(|(label, value)| Value::tuple(vec![label.clone(), value.clone()])),
            ))
        }),
        "keys" => series_method(this, name, |_, data, _| Ok(IndexObject::value(data.index))),
        "to_dict" => series_method(this, name, |_, data, _| {
            let mut dict = Dict::default();
            for (label, value) in data.index.values.iter().zip(data.values.iter()) {
                dict.entries.insert(Key::new(label.clone())?, value.clone());
            }
            Ok(Value::dict(dict))
        }),
        "copy" => series_method(this, name, |_, data, _| Ok(Series::from_data(data))),
        "head" => series_method(this, name, |_, data, args| {
            Ok(head_or_tail(data, &args, true))
        }),
        "tail" => series_method(this, name, |_, data, args| {
            Ok(head_or_tail(data, &args, false))
        }),
        "rename" => series_method(this, name, |_, mut data, args| {
            data.name = args.get(0).cloned().unwrap_or(Value::None);
            Ok(Series::from_data(data))
        }),
        "equals" => series_method(this, name, |_, data, args| {
            let Some(other) = args.get(0).and_then(|other| other.as_native::<Series>()) else {
                return Ok(Value::Bool(false));
            };
            let other = other.data();
            Ok(Value::Bool(
                other.values.len() == data.values.len()
                    && other
                        .values
                        .iter()
                        .zip(data.values.iter())
                        .all(|(a, b)| same_label(a, b)),
            ))
        }),
        "reset_index" => series_method(this, name, reset_index),
        "to_frame" => series_method(this, name, |_, data, args| {
            let name = args.get(0).cloned().unwrap_or(data.name.clone());
            let name = if name.is_none() { Value::Int(0) } else { name };
            Ok(DataFrame::from_data(FrameData::new(
                Labels::single(vec![name], Value::None),
                vec![data.values],
                data.index,
            )))
        }),
        // A label of the index, like a column of a row, is an attribute too.
        _ => data.values[*data.index.find(&Value::str(name)).first()?].clone(),
    })
}
