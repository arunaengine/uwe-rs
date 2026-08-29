//! The methods of `pandas.DataFrame` that reshape or filter the table: indexes, sorting,
//! dropping and renaming. Each takes the table's data and returns the changed data, so that
//! it can also work in place.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::frame::FrameData;
use super::labels::{Labels, labels_of};
use super::series::Series;
use super::{flag, is_list_like, is_missing, seq_values, value_text};
use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::value::{Args, Key, Value, key_equal};

/// The column labels a `subset=` or `by=` argument names: one label or a list of them.
fn column_list(interpreter: &mut Interpreter, value: &Value) -> Result<Vec<Value>, Error> {
    if is_list_like(value) {
        seq_values(interpreter, value)
    } else {
        Ok(vec![value.clone()])
    }
}

fn column_positions(
    interpreter: &mut Interpreter,
    data: &FrameData,
    value: &Value,
) -> Result<Vec<usize>, Error> {
    let mut positions = Vec::new();
    for label in column_list(interpreter, value)? {
        positions.push(
            data.column_position(&label)
                .ok_or_else(|| Error::key_error(label.clone()))?,
        );
    }
    Ok(positions)
}

/// `set_index(keys, drop=True, append=False)`.
pub fn set_index(
    interpreter: &mut Interpreter,
    data: FrameData,
    args: &mut Args,
) -> Result<FrameData, Error> {
    let keys = args
        .take(0, "keys")
        .ok_or_else(|| Error::type_error("set_index() missing keys"))?;
    let drop = flag(interpreter, args, "drop", true)?;
    let append = flag(interpreter, args, "append", false)?;
    args.take_keyword("verify_integrity");
    let keys = match keys.plain() {
        Value::List(_) => seq_values(interpreter, &keys)?,
        _ => vec![keys],
    };
    let mut levels: Vec<Vec<Value>> = Vec::new();
    let mut names: Vec<Value> = Vec::new();
    if append {
        let index = data.index();
        for level in 0..index.levels() {
            levels.push(index.level(level));
            names.push(index.names[level].clone());
        }
    }
    let mut dropped = Vec::new();
    for key in keys {
        if let Some(position) = data.column_position(&key)
            && !is_list_like(&key)
        {
            levels.push(data.data[position].clone());
            names.push(key);
            dropped.push(position);
            continue;
        }
        if is_list_like(&key) {
            let labels = labels_of(interpreter, &key)?;
            if labels.len() != data.rows() {
                return Err(Error::value_error("Length mismatch when setting the index"));
            }
            levels.push(labels.values);
            names.push(labels.names[0].clone());
            continue;
        }
        return Err(Error::key_error(Value::str(format!(
            "None of [{}] are in the columns",
            value_text(interpreter, &key, true)?
        ))));
    }
    let keep: Vec<usize> = data
        .all_columns()
        .into_iter()
        .filter(|position| !drop || !dropped.contains(position))
        .collect();
    let result = data.take_columns(&keep);
    *result.index.borrow_mut() = Labels::from_levels(levels, names);
    Ok(result)
}

/// `reset_index(drop=False)`: the levels of the index become the first columns.
pub fn reset_index(
    interpreter: &mut Interpreter,
    data: FrameData,
    args: &mut Args,
) -> Result<FrameData, Error> {
    args.take_keyword("level");
    let drop = flag(interpreter, args, "drop", false)?;
    let index = data.index();
    let mut result = data.copy();
    *result.index.borrow_mut() = Labels::range(index.len());
    if drop {
        return Ok(result);
    }
    let mut columns = Vec::new();
    let mut values = Vec::new();
    for level in 0..index.levels() {
        let name = index.names[level].clone();
        let name = if !name.is_none() {
            name
        } else if index.is_multi() {
            Value::str(format!("level_{level}"))
        } else if data.column_position(&Value::str("index")).is_some() {
            Value::str("level_0")
        } else {
            Value::str("index")
        };
        if data.column_position(&name).is_some() {
            return Err(Error::value_error(format!(
                "cannot insert {}, already exists",
                value_text(interpreter, &name, false)?
            )));
        }
        columns.push(name);
        values.push(index.level(level));
    }
    columns.extend(result.columns.values);
    values.extend(result.data);
    result.columns = Labels::single(columns, result.columns.names[0].clone());
    result.data = values;
    Ok(result)
}

pub fn sort_index(
    interpreter: &mut Interpreter,
    data: FrameData,
    args: &mut Args,
) -> Result<FrameData, Error> {
    let ascending = flag(interpreter, args, "ascending", true)?;
    let order = super::sorted_positions(interpreter, &data.index().values, !ascending)?;
    Ok(data.take_rows(&order))
}

/// `sort_values(by, ascending=True)`, stable as pandas' default sort for one key.
pub fn sort_values(
    interpreter: &mut Interpreter,
    data: FrameData,
    args: &mut Args,
) -> Result<FrameData, Error> {
    let by = args
        .take(0, "by")
        .ok_or_else(|| Error::type_error("sort_values() missing by"))?;
    let ascending = flag(interpreter, args, "ascending", true)?;
    let positions = column_positions(interpreter, &data, &by)?;
    let keys: Vec<Value> = (0..data.rows())
        .map(|row| {
            Value::tuple(
                positions
                    .iter()
                    .map(|column| data.data[*column][row].clone())
                    .collect(),
            )
        })
        .collect();
    let order = super::sorted_positions(interpreter, &keys, !ascending)?;
    Ok(data.take_rows(&order))
}

/// `dropna(axis=0, how="any", subset=None)`.
pub fn dropna(
    interpreter: &mut Interpreter,
    data: FrameData,
    args: &mut Args,
) -> Result<FrameData, Error> {
    let axis = args.take(0, "axis").unwrap_or(Value::Int(0));
    let how_all = matches!(
        args.take_keyword("how").as_ref().and_then(Value::as_str),
        Some("all")
    );
    let columns = match args.take_keyword("subset") {
        Some(subset) if !subset.is_none() => column_positions(interpreter, &data, &subset)?,
        _ => data.all_columns(),
    };
    let drop = |cells: &mut dyn Iterator<Item = &Value>| {
        let flags: Vec<bool> = cells.map(is_missing).collect();
        if how_all {
            !flags.is_empty() && flags.iter().all(|flag| *flag)
        } else {
            flags.iter().any(|flag| *flag)
        }
    };
    if matches!(axis.plain(), Value::Int(1)) || axis.as_str() == Some("columns") {
        let keep: Vec<usize> = data
            .all_columns()
            .into_iter()
            .filter(|column| !drop(&mut data.data[*column].iter()))
            .collect();
        return Ok(data.take_columns(&keep));
    }
    let keep: Vec<usize> = (0..data.rows())
        .filter(|row| !drop(&mut columns.iter().map(|column| &data.data[*column][*row])))
        .collect();
    Ok(data.take_rows(&keep))
}

/// `fillna(value)`: one value for all columns, or a dict of values by column.
pub fn fillna(
    interpreter: &mut Interpreter,
    mut data: FrameData,
    args: &mut Args,
) -> Result<FrameData, Error> {
    let fill = args.take(0, "value").unwrap_or(Value::None);
    for position in data.all_columns() {
        let value = match fill.plain() {
            Value::Dict(_) => {
                let label = data.columns.values[position].clone();
                match interpreter.get_item(&fill, &label) {
                    Ok(value) => value,
                    Err(error) if error.is("KeyError") => continue,
                    Err(error) => return Err(error),
                }
            }
            _ => fill.clone(),
        };
        for cell in &mut data.data[position] {
            if is_missing(cell) {
                *cell = value.clone();
            }
        }
    }
    Ok(data)
}

/// The rows that repeat an earlier row in the given columns.
fn repeated_rows(
    interpreter: &mut Interpreter,
    data: &FrameData,
    subset: Option<Value>,
) -> Result<Vec<Value>, Error> {
    let columns = match subset {
        Some(subset) if !subset.is_none() => column_positions(interpreter, data, &subset)?,
        _ => data.all_columns(),
    };
    let keys: Vec<Value> = (0..data.rows())
        .map(|row| {
            Value::tuple(
                columns
                    .iter()
                    .map(|column| data.data[*column][row].clone())
                    .collect(),
            )
        })
        .collect();
    Ok(super::duplicated(&keys))
}

pub fn drop_duplicates(
    interpreter: &mut Interpreter,
    data: FrameData,
    args: &mut Args,
) -> Result<FrameData, Error> {
    let subset = args.take(0, "subset");
    args.take_keyword("keep");
    args.take_keyword("ignore_index");
    let repeated = repeated_rows(interpreter, &data, subset)?;
    let keep: Vec<usize> = (0..data.rows())
        .filter(|row| matches!(repeated[*row], Value::Bool(false)))
        .collect();
    Ok(data.take_rows(&keep))
}

pub fn duplicated(
    interpreter: &mut Interpreter,
    data: FrameData,
    mut args: Args,
) -> Result<Value, Error> {
    let subset = args.take(0, "subset");
    let repeated = repeated_rows(interpreter, &data, subset)?;
    Ok(Series::value(repeated, data.index(), Value::None))
}

/// `drop(labels, axis=0, index=None, columns=None)`.
pub fn drop(
    interpreter: &mut Interpreter,
    data: FrameData,
    args: &mut Args,
) -> Result<FrameData, Error> {
    let labels = args.take(0, "labels");
    let axis = args.take(1, "axis").unwrap_or(Value::Int(0));
    let by_columns = matches!(axis.plain(), Value::Int(1)) || axis.as_str() == Some("columns");
    let mut rows = Vec::new();
    let mut columns = Vec::new();
    if let Some(labels) = labels.filter(|labels| !labels.is_none()) {
        if by_columns {
            columns = column_list(interpreter, &labels)?
        } else {
            rows = column_list(interpreter, &labels)?
        }
    }
    if let Some(value) = args.take_keyword("columns") {
        columns.extend(column_list(interpreter, &value)?);
    }
    if let Some(value) = args.take_keyword("index") {
        rows.extend(column_list(interpreter, &value)?);
    }
    args.take_keyword("errors");
    let keep_columns: Vec<usize> = data
        .all_columns()
        .into_iter()
        .filter(|position| {
            !columns
                .iter()
                .any(|label| key_equal(label, &data.columns.values[*position]))
        })
        .collect();
    let index = data.index();
    let keep_rows: Vec<usize> = (0..data.rows())
        .filter(|row| {
            !rows
                .iter()
                .any(|label| key_equal(label, &index.values[*row]))
        })
        .collect();
    Ok(data.take_columns(&keep_columns).take_rows(&keep_rows))
}

/// New labels from a dict or a function.
fn renamed(
    interpreter: &mut Interpreter,
    labels: &[Value],
    mapping: &Value,
) -> Result<Vec<Value>, Error> {
    let mut result = Vec::with_capacity(labels.len());
    for label in labels {
        let value = match mapping.plain() {
            Value::Dict(dict) => dict
                .borrow()
                .entries
                .get(&Key::new(label.clone())?)
                .cloned()
                .unwrap_or_else(|| label.clone()),
            _ => interpreter.call(mapping, Args::new(vec![label.clone()]))?,
        };
        result.push(value);
    }
    Ok(result)
}

pub fn rename(
    interpreter: &mut Interpreter,
    mut data: FrameData,
    args: &mut Args,
) -> Result<FrameData, Error> {
    if let Some(mapping) = args
        .take_keyword("columns")
        .or_else(|| args.positional.first().cloned())
    {
        data.columns.values = renamed(interpreter, &data.columns.values, &mapping)?;
    }
    if let Some(mapping) = args.take_keyword("index") {
        let mut index = data.index();
        index.values = renamed(interpreter, &index.values, &mapping)?;
        *data.index.borrow_mut() = index;
    }
    args.take_keyword("axis");
    Ok(data)
}

/// The table with rows and columns swapped.
pub fn transpose(data: &FrameData) -> FrameData {
    let index = data.index();
    let values = (0..data.rows())
        .map(|row| data.data.iter().map(|column| column[row].clone()).collect())
        .collect();
    FrameData::new(index, values, data.columns.clone())
}

/// `pandas.wide_to_long(df, stubnames, i, j, sep="", suffix=r"\d+")`: each column named after
/// a stub and a suffix becomes rows, one per suffix, indexed by the `i` columns and `j`, the
/// suffix; whole-number suffixes become numbers.
pub fn wide_to_long(interpreter: &mut Interpreter, mut args: Args) -> Result<Value, Error> {
    let frame = args.take(0, "df").unwrap_or(Value::None);
    let Some(frame) = frame
        .as_native::<super::DataFrame>()
        .map(super::DataFrame::data)
    else {
        return Err(Error::type_error("wide_to_long() needs a DataFrame"));
    };
    let stubs = args.take(1, "stubnames").unwrap_or(Value::None);
    let stubs: Vec<String> = column_list(interpreter, &stubs)?
        .iter()
        .map(|stub| interpreter.to_str(stub))
        .collect::<Result<_, _>>()?;
    let keys = args.take(2, "i").unwrap_or(Value::None);
    let keys = column_positions(interpreter, &frame, &keys)?;
    let j = args.take(3, "j").unwrap_or(Value::None);
    let separator = match args.take(4, "sep") {
        Some(value) => interpreter.to_str(&value)?,
        None => String::new(),
    };
    let suffix = match args.take(5, "suffix") {
        Some(value) => interpreter.to_str(&value)?,
        None => r"\d+".to_owned(),
    };
    // The columns of each stub by suffix, in the order of the table.
    let mut suffixes: Vec<String> = Vec::new();
    let mut found: Vec<(usize, usize, String)> = Vec::new();
    for (position, label) in frame.columns.values.iter().enumerate() {
        let name = interpreter.to_str(label)?;
        for (stub_index, stub) in stubs.iter().enumerate() {
            let pattern = format!(
                "^{}{}({suffix})$",
                fancy_regex::escape(stub),
                fancy_regex::escape(&separator)
            );
            let regex = fancy_regex::Regex::new(&pattern)
                .map_err(|error| Error::value_error(error.to_string()))?;
            if let Some(captures) = regex.captures(&name).ok().flatten() {
                let text = captures
                    .get(1)
                    .map_or("", |group| group.as_str())
                    .to_owned();
                if !suffixes.contains(&text) {
                    suffixes.push(text.clone());
                }
                found.push((stub_index, position, text));
            }
        }
    }
    let stub_columns: Vec<usize> = found.iter().map(|(_, position, _)| *position).collect();
    let others: Vec<usize> = (0..frame.columns.len())
        .filter(|position| !keys.contains(position) && !stub_columns.contains(position))
        .collect();
    let rows = frame.rows();
    let mut index_levels: Vec<Vec<Value>> = vec![Vec::new(); keys.len() + 1];
    let mut columns: Vec<Vec<Value>> = vec![Vec::new(); others.len() + stubs.len()];
    for row in 0..rows {
        for suffix in &suffixes {
            let number = suffix.parse::<i64>().ok();
            for (level, key) in keys.iter().enumerate() {
                index_levels[level].push(frame.data[*key][row].clone());
            }
            index_levels[keys.len()]
                .push(number.map_or_else(|| Value::str(suffix.as_str()), Value::Int));
            for (slot, other) in others.iter().enumerate() {
                columns[slot].push(frame.data[*other][row].clone());
            }
            for stub_index in 0..stubs.len() {
                let value = found
                    .iter()
                    .find(|(stub, _, text)| *stub == stub_index && text == suffix)
                    .map_or_else(super::nan, |(_, position, _)| {
                        frame.data[*position][row].clone()
                    });
                columns[others.len() + stub_index].push(value);
            }
        }
    }
    let mut names: Vec<Value> = keys
        .iter()
        .map(|key| frame.columns.values[*key].clone())
        .collect();
    names.push(j);
    let mut labels: Vec<Value> = others
        .iter()
        .map(|other| frame.columns.values[*other].clone())
        .collect();
    labels.extend(stubs.iter().map(|stub| Value::str(stub.as_str())));
    Ok(super::DataFrame::from_data(FrameData::new(
        Labels::single(labels, Value::None),
        columns,
        Labels::from_levels(index_levels, names),
    )))
}
