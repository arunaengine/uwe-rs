//! A stand-in for the part of pandas that workflows use while they are planned: reading
//! sample sheets into tables, and looking values up in them. It follows pandas 3.0: missing
//! values are `nan`, texts stay texts, and `.loc` selects by label.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod array;
mod combine;
mod construct;
mod display;
mod frame;
mod frame_methods;
mod group;
mod index;
mod indexer;
mod labels;
pub mod numpy;
mod options;
mod parse;
pub mod polars;
mod query;
mod read;
mod reshape;
mod sequence;
mod series;
mod series_methods;
mod strings;
mod values;

use std::rc::Rc;

pub use array::Array;
pub use frame::{DataFrame, FrameData};
pub use index::IndexObject;
pub use labels::Labels;
pub use series::{Series, SeriesData};

pub(crate) use display::{text_list, value_text};
pub(crate) use sequence::{
    is_list_like, is_mask, mask_or_positional_slice, positions_of, seq_values,
};
pub(crate) use values::{
    DtypeKind, cast_values, distinct, dtype_kind, dtype_name, duplicated, elementwise, extreme,
    map_values, replace_values, sorted_positions, sum, unary_values,
};

use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::modules::function;
use crate::value::{Args, Native, Value, key_equal};

pub fn module() -> Vec<(&'static str, Value)> {
    vec![
        ("read_csv", function("read_csv", read::read_csv)),
        ("read_table", function("read_table", read::read_table)),
        ("read_parquet", function("read_parquet", read::read_parquet)),
        (
            "DataFrame",
            Value::native(construct::Constructor::DataFrame),
        ),
        ("Series", Value::native(construct::Constructor::Series)),
        ("Index", Value::native(construct::Constructor::Index)),
        (
            "MultiIndex",
            Value::native(construct::Constructor::MultiIndex),
        ),
        (
            "isna",
            function("isna", |interpreter, args| {
                missing_test(interpreter, args, true)
            }),
        ),
        (
            "isnull",
            function("isnull", |interpreter, args| {
                missing_test(interpreter, args, true)
            }),
        ),
        (
            "notna",
            function("notna", |interpreter, args| {
                missing_test(interpreter, args, false)
            }),
        ),
        (
            "notnull",
            function("notnull", |interpreter, args| {
                missing_test(interpreter, args, false)
            }),
        ),
        ("concat", function("concat", combine::concat)),
        (
            "wide_to_long",
            function("wide_to_long", reshape::wide_to_long),
        ),
        (
            "merge",
            function("merge", |interpreter, mut args| {
                let left = args
                    .take(0, "left")
                    .ok_or_else(|| Error::type_error("merge() missing left"))?;
                let right = args
                    .take(1, "right")
                    .ok_or_else(|| Error::type_error("merge() missing right"))?;
                args.positional.clear();
                combine::merge(interpreter, left, right, args)
            }),
        ),
        (
            "unique",
            function("unique", |interpreter, args| {
                let values = seq_values(interpreter, args.get(0).unwrap_or(&Value::None))?;
                Ok(Array::value(distinct(&values)))
            }),
        ),
        ("to_numeric", function("to_numeric", values::to_numeric)),
        ("NA", nan()),
        ("NaT", nan()),
        ("set_option", function("set_option", |_, _| Ok(Value::None))),
        (
            "reset_option",
            function("reset_option", |_, _| Ok(Value::None)),
        ),
        ("options", options::options()),
        ("errors", options::errors()),
        ("api", options::api()),
        ("__version__", Value::str("3.0.6")),
    ]
}

/// Whether a value is missing: `None` or a float `nan`.
pub fn is_missing(value: &Value) -> bool {
    match value.plain() {
        Value::None => true,
        Value::Float(number) => number.is_nan(),
        _ => false,
    }
}

pub fn nan() -> Value {
    Value::Float(f64::NAN)
}

/// Whether two labels or values are the same, missing values alike.
pub fn same_label(a: &Value, b: &Value) -> bool {
    key_equal(a, b) || (is_missing(a) && is_missing(b))
}

/// A keyword argument that is a flag, like `drop=True`, or its default.
pub(crate) fn flag(
    interpreter: &mut Interpreter,
    args: &mut Args,
    name: &str,
    default: bool,
) -> Result<bool, Error> {
    match args.take_keyword(name) {
        Some(value) => interpreter.truthy(&value),
        None => Ok(default),
    }
}

/// A method of a pandas object: a builtin that calls `function` with the object.
pub(crate) fn method<T: Native + 'static>(
    this: &Value,
    name: &str,
    function: impl Fn(&mut Interpreter, &T, Args) -> Result<Value, Error> + 'static,
) -> Value {
    let this = this.clone();
    let owner = name.to_owned();
    Value::builtin(name, move |interpreter, args| {
        let Some(object) = this.as_native::<T>() else {
            return Err(Error::type_error(format!("{owner}() lost its object")));
        };
        function(interpreter, object, args)
    })
}

/// `pd.isna(value)` and its relatives, for one value or each value of a sequence.
fn missing_test(interpreter: &mut Interpreter, args: Args, missing: bool) -> Result<Value, Error> {
    let value = args.get(0).cloned().unwrap_or(Value::None);
    let test = |value: &Value| Value::Bool(is_missing(value) == missing);
    if let Some(series) = value.as_native::<Series>() {
        let data = series.data();
        return Ok(Series::from_data(
            data.with_values(data.values.iter().map(test).collect()),
        ));
    }
    if let Some(frame) = value.as_native::<DataFrame>() {
        return Ok(frame_methods::map_cells(&frame.data(), test));
    }
    if is_list_like(&value) {
        let values = seq_values(interpreter, &value)?;
        return Ok(Array::value(values.iter().map(test).collect()));
    }
    Ok(test(&value))
}

/// The rows of a data frame as dicts without missing values, or `None` for other values, as
/// Snakemake's `validate` checks them.
pub fn records(interpreter: &mut Interpreter, value: &Value) -> Result<Option<Vec<Value>>, Error> {
    let _ = interpreter;
    let Some(frame) = value.as_native::<DataFrame>() else {
        return Ok(None);
    };
    let data = frame.data();
    let mut records = Vec::with_capacity(data.rows());
    for row in 0..data.rows() {
        let mut dict = crate::value::Dict::default();
        for (label, column) in data.columns.values.iter().zip(data.data.iter()) {
            if !is_missing(&column[row]) {
                dict.entries
                    .insert(crate::value::Key::new(label.clone())?, column[row].clone());
            }
        }
        records.push(Value::dict(dict));
    }
    Ok(Some(records))
}

/// Takes the values of validated rows, which may hold defaults, into a data frame: new
/// columns start as `None`, and every value of a row replaces the table's value.
pub fn fill_from_records(
    interpreter: &mut Interpreter,
    frame: &Value,
    records: &[Value],
) -> Result<(), Error> {
    let Some(frame) = frame.as_native::<DataFrame>() else {
        return Ok(());
    };
    let mut data = frame.data.borrow_mut();
    for (row, record) in records.iter().enumerate() {
        for (key, value) in interpreter.mapping_items(record)? {
            if data.column_position(&key).is_none() {
                let rows = data.rows();
                data.set_column(key.clone(), vec![Value::None; rows]);
            }
            if let Some(column) = data.column_position(&key)
                && row < data.rows()
            {
                data.data[column][row] = value;
            }
        }
    }
    Ok(())
}

/// `lookup(query=..., within=frame)`: the rows of a data frame that a query selects, as
/// named tuples, or the values of `cols`, as Snakemake's `lookup` gives them.
pub fn lookup_query(
    interpreter: &mut Interpreter,
    within: &Value,
    query: &str,
    cols: Option<Value>,
    is_nrows: Option<Value>,
    default: Option<Value>,
) -> Result<Value, Error> {
    let data = match (
        within.as_native::<DataFrame>(),
        within.as_native::<Series>(),
    ) {
        (Some(frame), _) => frame.data(),
        (None, Some(series)) => {
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
        }
        _ => {
            return Err(Error::type_error(
                "lookup(query=...) needs a DataFrame or Series in within",
            ));
        }
    };
    let result = query::query(interpreter, &data, query)?;
    if let Some(rows) = is_nrows {
        return Ok(Value::Bool(rows.as_int() == Some(result.rows() as i64)));
    }
    if result.rows() == 0
        && let Some(default) = default
    {
        return Ok(default);
    }
    let with_index = cols.is_none();
    let result = match &cols {
        Some(cols) => {
            let frame = DataFrame::from_data(result);
            let selected = interpreter.get_item(&frame, cols)?;
            if let Some(series) = selected.as_native::<Series>() {
                return Ok(Value::list(series.data().values));
            }
            match selected.as_native::<DataFrame>() {
                Some(frame) => frame.data(),
                None => return Ok(selected),
            }
        }
        None => result,
    };
    let mut args = Args::default();
    args.keywords
        .push((Rc::from("index"), Value::Bool(with_index)));
    let tuples = frame_methods::itertuples(interpreter, result, args)?;
    let mut rows = interpreter.iterate(&tuples)?;
    if rows.len() == 1 {
        return Ok(rows.remove(0));
    }
    Ok(Value::list(rows))
}
