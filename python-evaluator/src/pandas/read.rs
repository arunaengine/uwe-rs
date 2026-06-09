//! `pandas.read_csv` and `pandas.read_table`: reads a delimited text file into a table and
//! infers the type of each column as pandas' C parser does.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::Read;

use super::frame::{DataFrame, FrameData};
use super::labels::Labels;
use super::parse::{Options, RawColumn, Separator, convert, infer, table};
use super::reshape::set_index;
use super::{dtype_kind, flag, seq_values};
use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::value::{Args, Value};

/// The texts that pandas reads as missing values by default.
const DEFAULT_MISSING: &[&str] = &[
    "", "#N/A", "#N/A N/A", "#NA", "-1.#IND", "-1.#QNAN", "-NaN", "-nan", "1.#IND", "1.#QNAN",
    "<NA>", "N/A", "NA", "NULL", "NaN", "None", "n/a", "nan", "null",
];

pub fn read_csv(interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    check_arguments("read_csv", &args)?;
    read(interpreter, args, ",")
}

pub fn read_table(interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    check_arguments("read_table", &args)?;
    read(interpreter, args, "\t")
}

/// `read_parquet(path)`: a missing file is an error as in pandas; reading the columnar
/// format is not supported.
pub fn read_parquet(interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    let source = args.get(0).cloned().ok_or_else(|| {
        Error::type_error("read_parquet() missing 1 required positional argument: 'path'")
    })?;
    let path = crate::modules::path_text(interpreter, &source)?;
    let resolved = interpreter.host.resolve(&path);
    if !path.contains("://") && interpreter.host.stat(&resolved).is_none() {
        return Err(Error::os_error(&std::io::ErrorKind::NotFound.into(), &path));
    }
    Err(Error::unsupported(
        "pandas.read_parquet: reading Parquet files",
    ))
}

/// The keyword arguments of `read_csv` and `read_table` in pandas 3.0.
const ARGUMENTS: &[&str] = &[
    "filepath_or_buffer",
    "sep",
    "delimiter",
    "header",
    "names",
    "index_col",
    "usecols",
    "dtype",
    "engine",
    "converters",
    "true_values",
    "false_values",
    "skipinitialspace",
    "skiprows",
    "skipfooter",
    "nrows",
    "na_values",
    "keep_default_na",
    "na_filter",
    "skip_blank_lines",
    "parse_dates",
    "date_format",
    "dayfirst",
    "cache_dates",
    "iterator",
    "chunksize",
    "compression",
    "thousands",
    "decimal",
    "lineterminator",
    "quotechar",
    "quoting",
    "doublequote",
    "escapechar",
    "comment",
    "encoding",
    "encoding_errors",
    "dialect",
    "on_bad_lines",
    "low_memory",
    "memory_map",
    "float_precision",
    "storage_options",
    "dtype_backend",
];

/// Arguments that pandas 3.0 no longer takes, like `delim_whitespace`, fail as in pandas.
fn check_arguments(function: &str, args: &Args) -> Result<(), Error> {
    match args
        .keywords
        .iter()
        .find(|(name, _)| !ARGUMENTS.contains(&&**name))
    {
        Some((name, _)) => Err(Error::type_error(format!(
            "{function}() got an unexpected keyword argument '{name}'"
        ))),
        None => Ok(()),
    }
}

/// The text of the source: a path, which may be compressed with gzip, or a file object.
fn source_text(interpreter: &mut Interpreter, source: &Value) -> Result<String, Error> {
    let is_path = matches!(source.plain(), Value::Str(_))
        || source.as_native::<crate::modules::PathObject>().is_some();
    if !is_path {
        let read = interpreter.get_attribute(source, "read")?;
        let text = interpreter.call(&read, Args::default())?;
        return interpreter.to_str(&text);
    }
    let path = crate::modules::path_text(interpreter, source)?;
    if path.contains("://") {
        return Err(Error::unsupported("pandas.read_csv of a URL"));
    }
    let resolved = interpreter.host.resolve(&path);
    let bytes = interpreter
        .host
        .read(&resolved)
        .map_err(|error| Error::os_error(&error, &path))?;
    let bytes = if path.ends_with(".gz") && bytes.starts_with(&[0x1f, 0x8b]) {
        let mut decoded = Vec::new();
        flate2::read::MultiGzDecoder::new(bytes.as_slice())
            .read_to_end(&mut decoded)
            .map_err(|error| Error::os_error(&error, &path))?;
        decoded
    } else {
        bytes
    };
    let text = String::from_utf8_lossy(&bytes).into_owned();
    Ok(text
        .strip_prefix('\u{feff}')
        .map(str::to_owned)
        .unwrap_or(text))
}

fn text_option(
    interpreter: &mut Interpreter,
    value: Option<Value>,
) -> Result<Option<String>, Error> {
    match value {
        Some(value) if !value.is_none() => Ok(Some(interpreter.to_str(&value)?)),
        _ => Ok(None),
    }
}

pub(super) fn read(
    interpreter: &mut Interpreter,
    mut args: Args,
    default_separator: &str,
) -> Result<Value, Error> {
    let source = args.take(0, "filepath_or_buffer").ok_or_else(|| {
        Error::type_error("read_csv() missing 1 required positional argument: 'filepath_or_buffer'")
    })?;
    let text = source_text(interpreter, &source)?;
    let separator = match args
        .take_keyword("sep")
        .or_else(|| args.take_keyword("delimiter"))
    {
        Some(value) if value.is_none() => Some(
            crate::modules::csv::sniff_delimiter(&text, "\t,;| ")
                .unwrap_or(',')
                .to_string(),
        ),
        Some(value) => Some(interpreter.to_str(&value)?),
        None => None,
    };
    let separator = separator.unwrap_or_else(|| default_separator.to_owned());
    let separator = if separator == r"\s+" {
        Separator::Whitespace
    } else if separator.chars().count() == 1 {
        Separator::Char(separator.chars().next().unwrap_or(','))
    } else if separator == r"\t" {
        Separator::Char('\t')
    } else {
        Separator::Pattern(crate::modules::compile_regex(&separator, 0)?)
    };
    let names = match args.take_keyword("names") {
        Some(value) if !value.is_none() => Some(seq_values(interpreter, &value)?),
        _ => None,
    };
    let header = match args.take_keyword("header") {
        Some(value) if value.is_none() => None,
        Some(Value::Int(row)) => Some(row.max(0) as usize),
        Some(value) if value.as_str() == Some("infer") => {
            if names.is_some() {
                None
            } else {
                Some(0)
            }
        }
        Some(value) => match seq_values(interpreter, &value)?
            .first()
            .and_then(Value::as_int)
        {
            Some(row) => Some(row.max(0) as usize),
            None => Some(0),
        },
        None => {
            if names.is_some() {
                None
            } else {
                Some(0)
            }
        }
    };
    let (skip_first, skip_rows) = match args.take_keyword("skiprows") {
        Some(Value::Int(count)) => (count.max(0) as usize, Vec::new()),
        Some(value) if value.is_none() => (0, Vec::new()),
        Some(value) => (
            0,
            seq_values(interpreter, &value)?
                .iter()
                .filter_map(Value::as_int)
                .map(|row| row.max(0) as usize)
                .collect(),
        ),
        None => (0, Vec::new()),
    };
    let quote = match args.take_keyword("quotechar") {
        Some(value) => text_option(interpreter, Some(value))?.and_then(|text| text.chars().next()),
        None => Some('"'),
    };
    let quote = match args.take_keyword("quoting") {
        Some(Value::Int(3)) => None,
        _ => quote,
    };
    let comment = text_option(interpreter, args.take_keyword("comment"))?
        .and_then(|text| text.chars().next());
    let options = Options {
        separator,
        quote,
        comment,
        header,
        names,
        skip_rows,
        skip_first,
        skip_initial_space: flag(interpreter, &mut args, "skipinitialspace", false)?,
        rows: args
            .take_keyword("nrows")
            .and_then(|value| value.as_int())
            .map(|rows| rows.max(0) as usize),
        skip_footer: args
            .take_keyword("skipfooter")
            .and_then(|value| value.as_int())
            .unwrap_or(0)
            .max(0) as usize,
    };
    let (columns, mut data, implicit_index) = table(&text, &options)?;
    let usecols = args
        .take_keyword("usecols")
        .filter(|value| !value.is_none());
    let (columns, data_columns) = match usecols {
        Some(usecols) => use_columns(interpreter, columns, std::mem::take(&mut data), &usecols)?,
        None => (columns, data),
    };
    // What counts as missing.
    let keep_default = flag(interpreter, &mut args, "keep_default_na", true)?;
    let filter = flag(interpreter, &mut args, "na_filter", true)?;
    let na_values = args
        .take_keyword("na_values")
        .filter(|value| !value.is_none());
    let dtype = args.take_keyword("dtype").filter(|value| !value.is_none());
    let converters = args
        .take_keyword("converters")
        .filter(|value| !value.is_none());
    let true_values = match args.take_keyword("true_values") {
        Some(value) if !value.is_none() => text_list(interpreter, &value)?,
        _ => Vec::new(),
    };
    let false_values = match args.take_keyword("false_values") {
        Some(value) if !value.is_none() => text_list(interpreter, &value)?,
        _ => Vec::new(),
    };
    let mut values = Vec::with_capacity(columns.len());
    for (position, (label, raw)) in columns.iter().zip(data_columns).enumerate() {
        let converter = match &converters {
            Some(converters) => lookup_column(interpreter, converters, label, position)?,
            None => None,
        };
        if let Some(converter) = converter {
            let mut converted = Vec::with_capacity(raw.len());
            for field in raw {
                converted.push(interpreter.call(
                    &converter,
                    Args::new(vec![Value::str(field.unwrap_or_default())]),
                )?);
            }
            values.push(converted);
            continue;
        }
        let mut missing: Vec<String> = if keep_default {
            DEFAULT_MISSING
                .iter()
                .map(|text| (*text).to_owned())
                .collect()
        } else {
            Vec::new()
        };
        if let Some(na_values) = &na_values {
            let extra = match na_values.plain() {
                Value::Dict(_) => match lookup_column(interpreter, na_values, label, position)? {
                    Some(extra) => text_list(interpreter, &extra)?,
                    None => Vec::new(),
                },
                _ => text_list(interpreter, na_values)?,
            };
            missing.extend(extra);
        }
        let kind = match &dtype {
            Some(dtype) => match dtype.plain() {
                Value::Dict(_) => match lookup_column(interpreter, dtype, label, position)? {
                    Some(kind) => dtype_kind(&kind),
                    None => None,
                },
                _ => dtype_kind(dtype),
            },
            None => None,
        };
        let fields: Vec<Option<String>> = raw
            .into_iter()
            .map(|field| {
                field.filter(|text| !(filter && missing.iter().any(|missing| missing == text)))
            })
            .collect();
        values.push(convert(label, fields, kind, &true_values, &false_values)?);
    }
    let rows = values
        .first()
        .map_or(implicit_index.as_ref().map_or(0, Vec::len), Vec::len);
    let index = match implicit_index {
        Some(labels) => Labels::single(
            infer(labels.into_iter().map(Some).collect(), &[], &[]),
            Value::None,
        ),
        None => Labels::range(rows),
    };
    let data = FrameData::new(Labels::single(columns, Value::None), values, index);
    let data = match args.take_keyword("index_col") {
        Some(index_col)
            if !index_col.is_none() && !matches!(index_col.plain(), Value::Bool(false)) =>
        {
            let keys = match index_col.plain() {
                Value::List(_) | Value::Tuple(_) => seq_values(interpreter, &index_col)?,
                _ => vec![index_col],
            };
            let keys: Vec<Value> = keys
                .into_iter()
                .map(|key| match key.plain() {
                    Value::Int(position) => data
                        .columns
                        .values
                        .get(*position as usize)
                        .cloned()
                        .unwrap_or(key.clone()),
                    _ => key.clone(),
                })
                .collect();
            let mut index_args = Args::new(vec![Value::list(keys)]);
            set_index(interpreter, data, &mut index_args)?
        }
        _ => data,
    };
    Ok(DataFrame::from_data(data))
}

fn text_list(interpreter: &mut Interpreter, value: &Value) -> Result<Vec<String>, Error> {
    let mut texts = Vec::new();
    for item in seq_values(interpreter, value)? {
        texts.push(interpreter.to_str(&item)?);
    }
    Ok(texts)
}

/// The entry of a dict for a column, by its label or its position.
fn lookup_column(
    interpreter: &mut Interpreter,
    dict: &Value,
    label: &Value,
    position: usize,
) -> Result<Option<Value>, Error> {
    for key in [label.clone(), Value::Int(position as i64)] {
        match interpreter.get_item(dict, &key) {
            Ok(value) => return Ok(Some(value)),
            Err(error) if error.is("KeyError") => {}
            Err(error) => return Err(error),
        }
    }
    Ok(None)
}

/// Keeps the columns `usecols` names, in the order of the file.
fn use_columns(
    interpreter: &mut Interpreter,
    columns: Vec<Value>,
    data: Vec<RawColumn>,
    usecols: &Value,
) -> Result<(Vec<Value>, Vec<RawColumn>), Error> {
    let mut keep = Vec::new();
    if interpreter.callable(usecols) {
        for (position, label) in columns.iter().enumerate() {
            let wanted = interpreter.call(usecols, Args::new(vec![label.clone()]))?;
            if interpreter.truthy(&wanted)? {
                keep.push(position);
            }
        }
    } else {
        let wanted = seq_values(interpreter, usecols)?;
        for (position, label) in columns.iter().enumerate() {
            if wanted.iter().any(|item| {
                crate::value::key_equal(item, label) || item.as_int() == Some(position as i64)
            }) {
                keep.push(position);
            }
        }
    }
    let columns = keep
        .iter()
        .map(|position| columns[*position].clone())
        .collect();
    let mut data = data;
    let data = keep
        .iter()
        .map(|position| std::mem::take(&mut data[*position]))
        .collect();
    Ok((columns, data))
}
