//! The classes of pandas that code calls to make objects (`DataFrame`, `Series`, `Index`,
//! `MultiIndex`), and the small parts of the module around them: `to_numeric`, `options`,
//! `errors` and `api.types`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::any::Any;

use super::frame::{DataFrame, FrameData};
use super::index::IndexObject;
use super::labels::{Labels, labels_of};
use super::series::Series;
use super::{is_list_like, nan, seq_values};
use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::modules::function;
use crate::value::{Args, Native, Value, key_equal};

#[derive(Clone, Copy)]
pub enum Constructor {
    DataFrame,
    Series,
    Index,
    MultiIndex,
}

impl Native for Constructor {
    fn type_name(&self) -> &str {
        "type"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn instance_of(&self, class: &str) -> bool {
        class == "callable"
    }

    fn check_instance(&self, value: &Value) -> Option<bool> {
        Some(match self {
            Constructor::DataFrame => value.as_native::<DataFrame>().is_some(),
            Constructor::Series => value.as_native::<Series>().is_some(),
            Constructor::Index => value.as_native::<IndexObject>().is_some(),
            Constructor::MultiIndex => value
                .as_native::<IndexObject>()
                .is_some_and(|index| index.labels.borrow().is_multi()),
        })
    }

    fn call(
        &self,
        interpreter: &mut Interpreter,
        _: &Value,
        args: Args,
    ) -> Result<Option<Value>, Error> {
        Ok(Some(match self {
            Constructor::DataFrame => data_frame(interpreter, args)?,
            Constructor::Series => series(interpreter, args)?,
            Constructor::Index => {
                let mut args = args;
                let data = args.take(0, "data").unwrap_or(Value::list(Vec::new()));
                let name = args
                    .take(1, "name")
                    .or_else(|| args.take_keyword("name"))
                    .unwrap_or(Value::None);
                let mut labels = labels_of(interpreter, &data)?;
                if !name.is_none() {
                    labels.names = vec![name];
                }
                IndexObject::value(labels)
            }
            Constructor::MultiIndex => {
                return Err(Error::unsupported(
                    "calling pandas.MultiIndex; use its from_ methods",
                ));
            }
        }))
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        _: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        Ok(match (self, name) {
            (Constructor::MultiIndex, "from_tuples") => {
                Some(function("from_tuples", |interpreter, mut args| {
                    let tuples = args.take(0, "tuples").unwrap_or(Value::None);
                    let names = args.take(1, "names");
                    let tuples = interpreter.iterate(&tuples)?;
                    let width = match tuples.first().map(Value::plain) {
                        Some(Value::Tuple(parts)) => parts.len(),
                        _ => 1,
                    };
                    let mut levels = vec![Vec::with_capacity(tuples.len()); width];
                    for tuple in &tuples {
                        let parts = interpreter.iterate(tuple)?;
                        for (level, part) in parts.into_iter().enumerate().take(width) {
                            levels[level].push(part);
                        }
                    }
                    multi_index(interpreter, levels, names)
                }))
            }
            (Constructor::MultiIndex, "from_arrays") => {
                Some(function("from_arrays", |interpreter, mut args| {
                    let arrays = args.take(0, "arrays").unwrap_or(Value::None);
                    let names = args.take(1, "names");
                    let mut levels = Vec::new();
                    for array in interpreter.iterate(&arrays)? {
                        levels.push(seq_values(interpreter, &array)?);
                    }
                    multi_index(interpreter, levels, names)
                }))
            }
            (Constructor::MultiIndex, "from_product") => {
                Some(function("from_product", |interpreter, mut args| {
                    let iterables = args.take(0, "iterables").unwrap_or(Value::None);
                    let names = args.take(1, "names");
                    let mut factors = Vec::new();
                    for iterable in interpreter.iterate(&iterables)? {
                        factors.push(seq_values(interpreter, &iterable)?);
                    }
                    let mut rows: Vec<Vec<Value>> = vec![Vec::new()];
                    for factor in &factors {
                        rows = rows
                            .into_iter()
                            .flat_map(|row| {
                                factor.iter().map(move |value| {
                                    let mut next = row.clone();
                                    next.push(value.clone());
                                    next
                                })
                            })
                            .collect();
                    }
                    let levels = (0..factors.len())
                        .map(|level| rows.iter().map(|row| row[level].clone()).collect())
                        .collect();
                    multi_index(interpreter, levels, names)
                }))
            }
            (Constructor::MultiIndex, "from_frame") => {
                Some(function("from_frame", |interpreter, mut args| {
                    let frame = args.take(0, "df").unwrap_or(Value::None);
                    let names = args.take(1, "names");
                    let Some(frame) = frame.as_native::<DataFrame>() else {
                        return Err(Error::type_error("Input must be a DataFrame"));
                    };
                    let data = frame.data();
                    let names = names.or_else(|| Some(Value::list(data.columns.values.clone())));
                    multi_index(interpreter, data.data, names)
                }))
            }
            (Constructor::DataFrame, "from_dict") => {
                Some(function("from_dict", |interpreter, mut args| {
                    let data = args.take(0, "data").unwrap_or(Value::None);
                    let orient = args
                        .take(1, "orient")
                        .and_then(|value| value.as_str().map(str::to_owned))
                        .unwrap_or_else(|| "columns".into());
                    let frame = data_frame(interpreter, Args::new(vec![data]))?;
                    if orient != "index" {
                        return Ok(frame);
                    }
                    let data = frame
                        .as_native::<DataFrame>()
                        .map(DataFrame::data)
                        .ok_or_else(|| Error::type_error("from_dict"))?;
                    Ok(DataFrame::from_data(super::reshape::transpose(&data)))
                }))
            }
            (Constructor::DataFrame, "from_records") => Some(function("from_records", data_frame)),
            _ => None,
        })
    }
}

fn multi_index(
    interpreter: &mut Interpreter,
    levels: Vec<Vec<Value>>,
    names: Option<Value>,
) -> Result<Value, Error> {
    let names = match names {
        Some(names) if !names.is_none() => seq_values(interpreter, &names)?,
        _ => vec![Value::None; levels.len()],
    };
    Ok(IndexObject::value(Labels::from_levels(levels, names)))
}

/// `pd.Series(data=None, index=None, name=None, dtype=None)`.
fn series(interpreter: &mut Interpreter, mut args: Args) -> Result<Value, Error> {
    let data = args.take(0, "data").unwrap_or(Value::None);
    let index = args.take(1, "index").filter(|index| !index.is_none());
    let dtype = args.take(2, "dtype").filter(|dtype| !dtype.is_none());
    let name = args.take(3, "name").unwrap_or(Value::None);
    let (mut values, mut labels) = match data.plain() {
        Value::Dict(_) => {
            let items = interpreter.mapping_items(&data)?;
            let labels = items.iter().map(|(key, _)| key.clone()).collect();
            (
                items.into_iter().map(|(_, value)| value).collect(),
                Labels::single(labels, Value::None),
            )
        }
        _ if data.as_native::<Series>().is_some() => {
            let series = data
                .as_native::<Series>()
                .map(Series::data)
                .ok_or_else(|| Error::type_error("Series"))?;
            (series.values, series.index)
        }
        Value::None => (Vec::new(), Labels::range(0)),
        _ if is_list_like(&data) => {
            let values = seq_values(interpreter, &data)?;
            let length = values.len();
            (values, Labels::range(length))
        }
        _ => (vec![data.clone()], Labels::range(1)),
    };
    if let Some(index) = index {
        let given = labels_of(interpreter, &index)?;
        if values.len() == 1 && given.len() != 1 && !is_list_like(&data) {
            values = vec![data.clone(); given.len()];
        } else if matches!(data.plain(), Value::Dict(_)) || data.as_native::<Series>().is_some() {
            values = given
                .values
                .iter()
                .map(|label| {
                    labels
                        .find(label)
                        .first()
                        .map_or_else(nan, |position| values[*position].clone())
                })
                .collect();
        }
        labels = given;
    }
    if let Some(dtype) = dtype {
        values = super::cast_values(interpreter, &values, &dtype)?;
    }
    Ok(Series::value(values, labels, name))
}

/// Rows given as dicts, series or sequences, as a table with columns in first-seen order.
fn from_rows(
    interpreter: &mut Interpreter,
    rows: &[Value],
    columns: Option<Vec<Value>>,
) -> Result<FrameData, Error> {
    let mut labels: Vec<Value> = columns.clone().unwrap_or_default();
    let mut records: Vec<Vec<(Value, Value)>> = Vec::with_capacity(rows.len());
    let mut row_labels: Vec<Value> = Vec::new();
    for (position, row) in rows.iter().enumerate() {
        let pairs: Vec<(Value, Value)> = if let Some(series) = row.as_native::<Series>() {
            let series = series.data();
            row_labels.push(if series.name.is_none() {
                Value::Int(position as i64)
            } else {
                series.name.clone()
            });
            series.index.values.into_iter().zip(series.values).collect()
        } else if let Value::Dict(_) = row.plain() {
            interpreter.mapping_items(row)?
        } else {
            let values = interpreter.iterate(row)?;
            values
                .into_iter()
                .enumerate()
                .map(|(column, value)| {
                    (
                        columns
                            .as_ref()
                            .and_then(|columns| columns.get(column).cloned())
                            .unwrap_or(Value::Int(column as i64)),
                        value,
                    )
                })
                .collect()
        };
        if columns.is_none() {
            for (key, _) in &pairs {
                if !labels.iter().any(|label| key_equal(label, key)) {
                    labels.push(key.clone());
                }
            }
        }
        records.push(pairs);
    }
    let data = labels
        .iter()
        .map(|label| {
            records
                .iter()
                .map(|pairs| {
                    pairs
                        .iter()
                        .find(|(key, _)| key_equal(key, label))
                        .map_or_else(nan, |(_, value)| value.clone())
                })
                .collect()
        })
        .collect();
    let index = if row_labels.len() == rows.len() && !rows.is_empty() {
        Labels::single(row_labels, Value::None)
    } else {
        Labels::range(rows.len())
    };
    Ok(FrameData::new(
        Labels::single(labels, Value::None),
        data,
        index,
    ))
}

/// `pd.DataFrame(data=None, index=None, columns=None, dtype=None)`.
pub fn data_frame(interpreter: &mut Interpreter, mut args: Args) -> Result<Value, Error> {
    let data = args.take(0, "data").unwrap_or(Value::None);
    let index = args.take(1, "index").filter(|index| !index.is_none());
    let columns = match args.take(2, "columns").filter(|columns| !columns.is_none()) {
        Some(columns) => Some(seq_values(interpreter, &columns)?),
        None => None,
    };
    let dtype = args.take(3, "dtype").filter(|dtype| !dtype.is_none());
    let mut frame = if let Some(frame) = data.as_native::<DataFrame>() {
        frame.data().copy()
    } else if let Some(series) = data.as_native::<Series>() {
        let series = series.data();
        let name = if series.name.is_none() {
            Value::Int(0)
        } else {
            series.name.clone()
        };
        FrameData::new(
            Labels::single(vec![name], Value::None),
            vec![series.values],
            series.index,
        )
    } else if let Value::Dict(_) = data.plain() {
        from_columns(interpreter, &data, index.as_ref())?
    } else if data.is_none() {
        let columns = columns.clone().unwrap_or_default();
        let width = columns.len();
        let rows = match &index {
            Some(index) => labels_of(interpreter, index)?,
            None => Labels::range(0),
        };
        let length = rows.len();
        FrameData::new(
            Labels::single(columns, Value::None),
            vec![vec![nan(); length]; width],
            rows,
        )
    } else {
        let rows = interpreter.iterate(&data)?;
        from_rows(interpreter, &rows, columns.clone())?
    };
    if let Some(index) = index {
        let labels = labels_of(interpreter, &index)?;
        if labels.len() == frame.rows() {
            *frame.index.borrow_mut() = labels;
        }
    }
    if let Some(columns) = columns
        && matches!(data.plain(), Value::Dict(_))
    {
        let rows = frame.rows();
        let mut ordered = FrameData::new(
            Labels::single(Vec::new(), Value::None),
            Vec::new(),
            frame.index(),
        );
        for label in columns {
            let values = match frame.column_position(&label) {
                Some(position) => frame.data[position].clone(),
                None => vec![nan(); rows],
            };
            ordered.set_column(label, values);
        }
        frame = ordered;
    }
    if let Some(dtype) = dtype {
        for position in frame.all_columns() {
            frame.data[position] = super::cast_values(interpreter, &frame.data[position], &dtype)?;
        }
    }
    Ok(DataFrame::from_data(frame))
}

/// The values of a column as pandas 3 infers its type: `None` among texts or numbers is
/// missing, and whole numbers among missing values become floats.
fn infer_column(values: Vec<Value>) -> Vec<Value> {
    let present: Vec<&Value> = values
        .iter()
        .filter(|value| !super::is_missing(value))
        .collect();
    if present.len() == values.len() || present.is_empty() {
        return values;
    }
    let texts = present
        .iter()
        .all(|value| matches!(value.plain(), Value::Str(_)));
    let numbers = present
        .iter()
        .all(|value| matches!(value.plain(), Value::Int(_) | Value::Float(_)));
    values
        .into_iter()
        .map(|value| match value.plain() {
            _ if super::is_missing(&value) && (texts || numbers) => nan(),
            Value::Int(number) if numbers => Value::Float(*number as f64),
            _ => value,
        })
        .collect()
}

/// A table from a dict of columns: sequences, series, dicts by row label, or single values.
fn from_columns(
    interpreter: &mut Interpreter,
    data: &Value,
    index: Option<&Value>,
) -> Result<FrameData, Error> {
    let items = interpreter.mapping_items(data)?;
    let mut labels: Option<Labels> = match index {
        Some(index) => Some(labels_of(interpreter, index)?),
        None => None,
    };
    // Row labels come from the first series or dict, or count the rows.
    if labels.is_none() {
        for (_, value) in &items {
            if let Some(series) = value.as_native::<Series>() {
                labels = Some(series.data().index);
                break;
            }
            if let Value::Dict(_) = value.plain() {
                let keys = interpreter
                    .mapping_items(value)?
                    .into_iter()
                    .map(|(key, _)| key)
                    .collect();
                labels = Some(Labels::single(keys, Value::None));
                break;
            }
        }
    }
    if labels.is_none() {
        let length = items
            .iter()
            .find(|(_, value)| is_list_like(value))
            .map(|(_, value)| seq_values(interpreter, value).map(|values| values.len()))
            .transpose()?
            .unwrap_or(1);
        labels = Some(Labels::range(length));
    }
    let labels = labels.unwrap_or_else(|| Labels::range(0));
    let mut columns = Vec::with_capacity(items.len());
    let mut values = Vec::with_capacity(items.len());
    for (key, value) in items {
        let column = if let Some(series) = value.as_native::<Series>() {
            let series = series.data();
            labels
                .values
                .iter()
                .map(|label| {
                    series
                        .index
                        .find(label)
                        .first()
                        .map_or_else(nan, |position| series.values[*position].clone())
                })
                .collect()
        } else if let Value::Dict(_) = value.plain() {
            let pairs = interpreter.mapping_items(&value)?;
            labels
                .values
                .iter()
                .map(|label| {
                    pairs
                        .iter()
                        .find(|(key, _)| key_equal(key, label))
                        .map_or_else(nan, |(_, value)| value.clone())
                })
                .collect()
        } else if is_list_like(&value) {
            let column = seq_values(interpreter, &value)?;
            if column.len() != labels.len() {
                return Err(Error::value_error("All arrays must be of the same length"));
            }
            infer_column(column)
        } else {
            vec![value.clone(); labels.len()]
        };
        columns.push(key);
        values.push(column);
    }
    Ok(FrameData::new(
        Labels::single(columns, Value::None),
        values,
        labels,
    ))
}
