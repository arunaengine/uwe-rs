//! The methods of `pandas.DataFrame`. Those that may work in place take the table's data and
//! return the changed data.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::frame::{DataFrame, FrameData};
use super::series::Series;
use super::{flag, is_missing, seq_values, value_text};
use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::value::{Args, Dict, Key, Value, key_equal};

/// `astype(dtype)`: one type for all columns, or a dict of types by column.
pub fn astype(
    interpreter: &mut Interpreter,
    data: FrameData,
    mut args: Args,
) -> Result<Value, Error> {
    let kind = args.take(0, "dtype").unwrap_or(Value::None);
    let mut result = data.copy();
    for position in result.all_columns() {
        let target = match kind.plain() {
            Value::Dict(_) => match interpreter.get_item(&kind, &result.columns.values[position]) {
                Ok(value) => value,
                Err(error) if error.is("KeyError") => continue,
                Err(error) => return Err(error),
            },
            _ => kind.clone(),
        };
        result.data[position] = super::cast_values(interpreter, &result.data[position], &target)?;
    }
    Ok(DataFrame::from_data(result))
}

pub fn isin(interpreter: &mut Interpreter, data: FrameData, args: Args) -> Result<Value, Error> {
    let wanted = args.get(0).cloned().unwrap_or(Value::None);
    let mut result = data.copy();
    for position in result.all_columns() {
        let values = match wanted.plain() {
            Value::Dict(_) => match interpreter.get_item(&wanted, &result.columns.values[position])
            {
                Ok(values) => seq_values(interpreter, &values)?,
                Err(_) => Vec::new(),
            },
            _ => seq_values(interpreter, &wanted)?,
        };
        for cell in &mut result.data[position] {
            *cell = Value::Bool(values.iter().any(|value| super::same_label(value, cell)));
        }
    }
    Ok(DataFrame::from_data(result))
}

/// The values of a column of a table from a series, a sequence or one value.
pub fn column_values(
    interpreter: &mut Interpreter,
    data: &FrameData,
    value: &Value,
) -> Result<Vec<Value>, Error> {
    let rows = data.rows();
    if let Some(series) = value.as_native::<Series>() {
        let series = series.data();
        let index = data.index();
        if series.index.values.len() == rows
            && series
                .index
                .values
                .iter()
                .zip(index.values.iter())
                .all(|(a, b)| key_equal(a, b))
        {
            return Ok(series.values);
        }
        // Aligned by label; labels the series lacks get a missing value.
        return Ok(index
            .values
            .iter()
            .map(|label| {
                series
                    .index
                    .find(label)
                    .first()
                    .map_or_else(super::nan, |position| series.values[*position].clone())
            })
            .collect());
    }
    if super::is_list_like(value) {
        let values = super::seq_values(interpreter, value)?;
        if values.len() != rows {
            return Err(Error::value_error(format!(
                "Length of values ({}) does not match length of index ({rows})",
                values.len()
            )));
        }
        return Ok(values);
    }
    Ok(vec![value.clone(); rows])
}

/// A table of the same shape with a function applied to every cell.
pub fn map_cells(data: &FrameData, function: impl Fn(&Value) -> Value) -> Value {
    let mut result = data.copy();
    for column in &mut result.data {
        for cell in column.iter_mut() {
            *cell = function(cell);
        }
    }
    DataFrame::from_data(result)
}

/// `apply(func, axis=0)`: calls the function with each column, or with each row for
/// `axis=1`. Results that are series make a table.
pub fn apply(
    interpreter: &mut Interpreter,
    data: FrameData,
    mut args: Args,
) -> Result<Value, Error> {
    let function = args
        .take(0, "func")
        .ok_or_else(|| Error::type_error("apply() missing func"))?;
    let axis = args.take(1, "axis").unwrap_or(Value::Int(0));
    let by_rows = matches!(axis.plain(), Value::Int(1)) || axis.as_str() == Some("columns");
    let extra: Vec<Value> = match args.take_keyword("args") {
        Some(extra) => interpreter.iterate(&extra)?,
        None => Vec::new(),
    };
    let (count, labels) = if by_rows {
        (data.rows(), data.index())
    } else {
        (data.columns.len(), data.columns.clone())
    };
    let columns = data.all_columns();
    let mut results = Vec::with_capacity(count);
    for position in 0..count {
        let item = if by_rows {
            data.row(position, &columns)
        } else {
            data.column(position)
        };
        let mut call = Args::new(vec![Series::from_data(item)]);
        call.positional.extend(extra.iter().cloned());
        call.keywords = args.keywords.clone();
        results.push(interpreter.call(&function, call)?);
    }
    if !results.is_empty()
        && results
            .iter()
            .all(|result| result.as_native::<Series>().is_some())
    {
        let rows: Vec<_> = results
            .iter()
            .filter_map(|result| result.as_native::<Series>())
            .map(Series::data)
            .collect();
        let columns = rows[0].index.clone();
        let values = (0..columns.len())
            .map(|column| {
                rows.iter()
                    .map(|row| row.values.get(column).cloned().unwrap_or_else(super::nan))
                    .collect()
            })
            .collect();
        return Ok(DataFrame::from_data(FrameData::new(
            columns, values, labels,
        )));
    }
    Ok(Series::value(results, labels, Value::None))
}

/// `itertuples(index=True, name="Pandas")`: one named tuple per row.
pub fn itertuples(
    interpreter: &mut Interpreter,
    data: FrameData,
    mut args: Args,
) -> Result<Value, Error> {
    let with_index = flag(interpreter, &mut args, "index", true)?;
    let name = match args.take_keyword("name") {
        Some(value) if value.is_none() => None,
        Some(value) => Some(interpreter.to_str(&value)?),
        None => Some("Pandas".to_owned()),
    };
    let mut fields: Vec<String> = Vec::new();
    if with_index {
        fields.push("Index".into());
    }
    for (position, label) in data.columns.values.iter().enumerate() {
        let text = value_text(interpreter, label, false)?;
        let valid = text
            .chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_')
            && text.chars().all(|c| c.is_alphanumeric() || c == '_')
            && !fields.contains(&text);
        // Labels that are no identifiers get their position, as namedtuple's rename does.
        fields.push(if valid {
            text
        } else {
            format!("_{}", position + usize::from(with_index))
        });
    }
    let index = data.index();
    let mut rows = Vec::with_capacity(data.rows());
    for row in 0..data.rows() {
        let mut values = Vec::with_capacity(fields.len());
        if with_index {
            values.push(index.values[row].clone());
        }
        values.extend(data.data.iter().map(|column| column[row].clone()));
        rows.push(values);
    }
    Ok(Value::iterator(match &name {
        Some(name) => crate::modules::collections::named_tuples(name, &fields, rows),
        None => rows.into_iter().map(Value::tuple).collect(),
    }))
}

fn dict_of(entries: impl IntoIterator<Item = (Value, Value)>) -> Result<Value, Error> {
    let mut dict = Dict::default();
    for (key, value) in entries {
        dict.entries.insert(Key::new(key)?, value);
    }
    Ok(Value::dict(dict))
}

/// `to_dict(orient="dict")`, also with the orients `list`, `records`, `index` and `series`.
pub fn to_dict(
    interpreter: &mut Interpreter,
    data: FrameData,
    mut args: Args,
) -> Result<Value, Error> {
    let orient = match args.take(0, "orient") {
        Some(value) => interpreter.to_str(&value)?,
        None => "dict".into(),
    };
    let index = data.index();
    let columns = data.all_columns();
    match orient.as_str() {
        "dict" => dict_of(columns.iter().map(|column| {
            let inner = dict_of(
                index
                    .values
                    .iter()
                    .cloned()
                    .zip(data.data[*column].iter().cloned()),
            );
            (
                data.columns.values[*column].clone(),
                inner.unwrap_or(Value::None),
            )
        })),
        "list" => dict_of(columns.iter().map(|column| {
            (
                data.columns.values[*column].clone(),
                Value::list(data.data[*column].clone()),
            )
        })),
        "series" => dict_of(columns.iter().map(|column| {
            (
                data.columns.values[*column].clone(),
                Series::from_data(data.column(*column)),
            )
        })),
        "records" => {
            let mut records = Vec::with_capacity(data.rows());
            for row in 0..data.rows() {
                records.push(dict_of(
                    data.columns
                        .values
                        .iter()
                        .cloned()
                        .zip(data.data.iter().map(|column| column[row].clone())),
                )?);
            }
            Ok(Value::list(records))
        }
        "index" => {
            let mut entries = Vec::with_capacity(data.rows());
            for row in 0..data.rows() {
                let inner = dict_of(
                    data.columns
                        .values
                        .iter()
                        .cloned()
                        .zip(data.data.iter().map(|column| column[row].clone())),
                )?;
                entries.push((index.values[row].clone(), inner));
            }
            dict_of(entries)
        }
        other => Err(Error::value_error(format!(
            "orient '{other}' not understood"
        ))),
    }
}

/// `assign(name=value, ...)`: a copy with new or changed columns; functions get the table.
pub fn assign(interpreter: &mut Interpreter, data: FrameData, args: Args) -> Result<Value, Error> {
    let mut result = data.copy();
    for (name, value) in args.keywords {
        let value = if interpreter.callable(&value) {
            interpreter.call(&value, Args::new(vec![DataFrame::from_data(result.copy())]))?
        } else {
            value
        };
        let values = column_values(interpreter, &result, &value)?;
        result.set_column(Value::str(&*name), values);
    }
    Ok(DataFrame::from_data(result))
}

/// `filter(items=None, like=None, regex=None)` over the column labels.
pub fn filter(
    interpreter: &mut Interpreter,
    data: FrameData,
    mut args: Args,
) -> Result<Value, Error> {
    let items = args.take(0, "items");
    let like = args.take_keyword("like");
    let regex = args.take_keyword("regex");
    let mut keep = Vec::new();
    for (position, label) in data.columns.values.iter().enumerate() {
        let text = value_text(interpreter, label, false)?;
        let wanted = if let Some(items) = items.as_ref().filter(|items| !items.is_none()) {
            seq_values(interpreter, items)?
                .iter()
                .any(|item| key_equal(item, label))
        } else if let Some(like) = &like {
            text.contains(&interpreter.to_str(like)?)
        } else if let Some(regex) = &regex {
            let pattern = crate::modules::compile_regex(&interpreter.to_str(regex)?, 0)?;
            pattern.search_regex().is_match(&text).unwrap_or(false)
        } else {
            true
        };
        if wanted {
            keep.push(position);
        }
    }
    Ok(DataFrame::from_data(data.take_columns(&keep)))
}

/// `update(other)`: takes the values of another table that are not missing, by labels.
pub fn update(
    interpreter: &mut Interpreter,
    frame: &DataFrame,
    args: Args,
) -> Result<Value, Error> {
    let other = args.get(0).cloned().unwrap_or(Value::None);
    let other = match other.as_native::<DataFrame>() {
        Some(other) => other.data(),
        None => {
            let _ = interpreter;
            return Err(Error::unsupported(
                "DataFrame.update() with other than a DataFrame",
            ));
        }
    };
    let other_index = other.index();
    let mut data = frame.data.borrow_mut();
    let index = data.index();
    for (other_column, label) in other.columns.values.iter().enumerate() {
        let Some(column) = data.column_position(label) else {
            continue;
        };
        for (other_row, row_label) in other_index.values.iter().enumerate() {
            let value = &other.data[other_column][other_row];
            if is_missing(value) {
                continue;
            }
            for row in index.find(row_label) {
                data.data[column][row] = value.clone();
            }
        }
    }
    Ok(Value::None)
}

/// `insert(loc, column, value)`.
pub fn insert(
    interpreter: &mut Interpreter,
    frame: &DataFrame,
    mut args: Args,
) -> Result<Value, Error> {
    let position = args
        .take(0, "loc")
        .and_then(|value| value.as_int())
        .unwrap_or(0)
        .max(0) as usize;
    let label = args.take(1, "column").unwrap_or(Value::None);
    let value = args.take(2, "value").unwrap_or(Value::None);
    let values = frame.column_values(interpreter, &value)?;
    let mut data = frame.data.borrow_mut();
    if data.column_position(&label).is_some() {
        return Err(Error::value_error(format!(
            "cannot insert {}, already exists",
            crate::format::plain_str(&label)
        )));
    }
    let position = position.min(data.columns.len());
    data.columns.values.insert(position, label);
    data.data.insert(position, values);
    Ok(Value::None)
}

/// `to_csv(path=None, sep=",", index=True, header=True)`: the text, or a write to the path.
pub fn to_csv(
    interpreter: &mut Interpreter,
    data: FrameData,
    mut args: Args,
) -> Result<Value, Error> {
    let path = args.take(0, "path_or_buf");
    let sep = match args.take_keyword("sep") {
        Some(value) => interpreter.to_str(&value)?,
        None => ",".into(),
    };
    let with_index = flag(interpreter, &mut args, "index", true)?;
    let with_header = flag(interpreter, &mut args, "header", true)?;
    let index = data.index();
    let mut text = String::new();
    let field = |interpreter: &mut Interpreter, value: &Value| -> Result<String, Error> {
        if is_missing(value) {
            Ok(String::new())
        } else {
            interpreter.to_str(value)
        }
    };
    if with_header {
        let mut cells = Vec::new();
        if with_index {
            cells.push(field(interpreter, &index.names[0])?);
        }
        for label in &data.columns.values {
            cells.push(field(interpreter, label)?);
        }
        text.push_str(&cells.join(&sep));
        text.push('\n');
    }
    for row in 0..data.rows() {
        let mut cells = Vec::new();
        if with_index {
            cells.push(field(interpreter, &index.values[row])?);
        }
        for column in &data.data {
            cells.push(field(interpreter, &column[row])?);
        }
        text.push_str(&cells.join(&sep));
        text.push('\n');
    }
    match path.filter(|path| !path.is_none()) {
        None => Ok(Value::str(text)),
        Some(path) => {
            let path = crate::modules::path_text(interpreter, &path)?;
            let resolved = interpreter.host.resolve(&path);
            interpreter
                .host
                .write(&resolved, text.as_bytes(), false)
                .map_err(|error| Error::os_error(&error, &path))?;
            Ok(Value::None)
        }
    }
}

/// `nunique()`, `count()`, `any()` and `all()` of each column.
pub fn reduce_columns(
    interpreter: &mut Interpreter,
    data: &FrameData,
    which: &str,
) -> Result<Value, Error> {
    let mut results = Vec::with_capacity(data.columns.len());
    for column in &data.data {
        let present: Vec<Value> = column
            .iter()
            .filter(|value| !is_missing(value))
            .cloned()
            .collect();
        results.push(match which {
            "nunique" => Value::Int(super::distinct(&present).len() as i64),
            "count" => Value::Int(present.len() as i64),
            "any" => {
                let mut found = false;
                for value in &present {
                    found |= interpreter.truthy(value)?;
                }
                Value::Bool(found)
            }
            _ => {
                let mut all = true;
                for value in &present {
                    all &= interpreter.truthy(value)?;
                }
                Value::Bool(all)
            }
        });
    }
    Ok(Series::value(results, data.columns.clone(), Value::None))
}
