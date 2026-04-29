//! Splits the text of a delimited file into columns, and infers the type of each column as
//! pandas' C parser does.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::{DtypeKind, nan};
use crate::error::Error;
use crate::value::Value;

pub(super) enum Separator {
    Char(char),
    Whitespace,
    Pattern(std::rc::Rc<crate::modules::Pattern>),
}

/// The options of a read that shape the rows and columns.
pub(super) struct Options {
    pub(super) separator: Separator,
    pub(super) quote: Option<char>,
    pub(super) comment: Option<char>,
    pub(super) header: Option<usize>,
    pub(super) names: Option<Vec<Value>>,
    pub(super) skip_rows: Vec<usize>,
    pub(super) skip_first: usize,
    pub(super) skip_initial_space: bool,
    pub(super) rows: Option<usize>,
    pub(super) skip_footer: usize,
}

/// Splits one record into fields.
fn fields(record: &str, options: &Options) -> Vec<String> {
    let mut fields = match &options.separator {
        Separator::Char(separator) => {
            crate::modules::data::split_csv_line(record, *separator, options.quote)
        }
        Separator::Whitespace => record.split_whitespace().map(str::to_owned).collect(),
        Separator::Pattern(pattern) => {
            let mut parts = Vec::new();
            let mut last = 0;
            for found in pattern.search_regex().find_iter(record).flatten() {
                parts.push(record[last..found.start()].to_owned());
                last = found.end();
            }
            parts.push(record[last..].to_owned());
            parts
        }
    };
    if options.skip_initial_space {
        for field in &mut fields {
            *field = field.trim_start().to_owned();
        }
    }
    fields
}

/// The records of the text: lines, joined where a quoted field holds a line break, without
/// comments, skipped rows and blank lines.
fn records(text: &str, options: &Options) -> Vec<(usize, String)> {
    let mut records = Vec::new();
    let mut pending = String::new();
    let mut first_line = 0;
    for (number, line) in text.lines().enumerate() {
        if pending.is_empty() {
            first_line = number;
        } else {
            pending.push('\n');
        }
        pending.push_str(line);
        if let Some(quote) = options.quote
            && pending.matches(quote).count() % 2 == 1
        {
            continue;
        }
        let record = std::mem::take(&mut pending);
        if first_line < options.skip_first || options.skip_rows.contains(&first_line) {
            continue;
        }
        let record = match options.comment {
            Some(comment) => match record.find(comment) {
                Some(cut) => record[..cut].to_owned(),
                None => record,
            },
            None => record,
        };
        let blank = match options.separator {
            Separator::Whitespace => record.trim().is_empty(),
            _ => record.is_empty(),
        };
        if !blank {
            records.push((first_line + 1, record));
        }
    }
    if !pending.is_empty() {
        records.push((first_line + 1, pending));
    }
    records
}

/// The fields of a column as read, `None` where a row has no field for it.
pub(super) type RawColumn = Vec<Option<String>>;

/// Column labels and raw fields by column, and the labels of an index that the rows hold
/// in a first column the header lacks.
type Table = (Vec<Value>, Vec<RawColumn>, Option<Vec<String>>);

pub(super) fn table(text: &str, options: &Options) -> Result<Table, Error> {
    let records = records(text, options);
    let mut rows: Vec<(usize, Vec<String>)> = records
        .iter()
        .map(|(line, record)| (*line, fields(record, options)))
        .collect();
    let header: Option<Vec<String>> = match options.header {
        Some(row) if row < rows.len() => {
            let header = rows[row].1.clone();
            rows.drain(..=row);
            Some(header)
        }
        Some(_) => {
            if options.names.is_none() {
                return Err(Error::new(
                    "EmptyDataError",
                    "No columns to parse from file",
                ));
            }
            rows.clear();
            None
        }
        None => None,
    };
    if options.skip_footer > 0 {
        let keep = rows.len().saturating_sub(options.skip_footer);
        rows.truncate(keep);
    }
    if let Some(limit) = options.rows {
        rows.truncate(limit);
    }
    let width = rows
        .iter()
        .map(|(_, fields)| fields.len())
        .max()
        .unwrap_or(0);
    let mut columns: Vec<Value> = match (&options.names, &header) {
        (Some(names), _) => names.clone(),
        (None, Some(header)) => labels_of_header(header),
        (None, None) => (0..width as i64).map(Value::Int).collect(),
    };
    // Rows with one field more than the header hold the index in their first field.
    let implicit = header.is_some()
        && options.names.is_none()
        && !rows.is_empty()
        && rows
            .iter()
            .all(|(_, fields)| fields.len() == columns.len() + 1);
    let mut index = None;
    if implicit {
        index = Some(
            rows.iter_mut()
                .map(|(_, fields)| fields.remove(0))
                .collect(),
        );
    }
    if options.names.is_some() && width > columns.len() {
        // Extra leading fields become the index, as pandas does with names shorter than rows.
        let extra = width - columns.len();
        if extra == 1 {
            index = Some(
                rows.iter_mut()
                    .map(|(_, fields)| {
                        if fields.len() > columns.len() {
                            fields.remove(0)
                        } else {
                            String::new()
                        }
                    })
                    .collect(),
            );
        }
    }
    for (line, fields) in &rows {
        if fields.len() > columns.len() {
            if header.is_none() && options.names.is_none() {
                continue;
            }
            return Err(Error::new(
                "ParserError",
                format!(
                    "Error tokenizing data. C error: Expected {} fields in line {line}, saw {}",
                    columns.len(),
                    fields.len()
                ),
            ));
        }
    }
    if header.is_none() && options.names.is_none() {
        columns = (0..width as i64).map(Value::Int).collect();
    }
    let data = (0..columns.len())
        .map(|column| {
            rows.iter()
                .map(|(_, fields)| fields.get(column).cloned())
                .collect()
        })
        .collect();
    Ok((columns, data, index))
}

/// Column labels from a header row: empty ones are `Unnamed: <position>`, and repeated ones
/// get `.1`, `.2` and so on.
fn labels_of_header(header: &[String]) -> Vec<Value> {
    let mut labels: Vec<String> = Vec::with_capacity(header.len());
    for (position, name) in header.iter().enumerate() {
        let name = if name.is_empty() {
            format!("Unnamed: {position}")
        } else {
            name.clone()
        };
        let mut label = name.clone();
        let mut count = 1;
        while labels.contains(&label) {
            label = format!("{name}.{count}");
            count += 1;
        }
        labels.push(label);
    }
    labels.into_iter().map(Value::str).collect()
}

fn parse_int(text: &str) -> Option<i64> {
    let text = text.trim();
    let digits = text.strip_prefix(['+', '-']).unwrap_or(text);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

fn parse_float(text: &str) -> Option<f64> {
    let text = text.trim();
    let lower = text.to_ascii_lowercase();
    let body = lower.trim_start_matches(['+', '-']);
    if body == "inf" || body == "infinity" {
        return text.parse().ok();
    }
    if !body.bytes().any(|byte| byte.is_ascii_digit())
        || !body
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'.' | b'e' | b'+' | b'-'))
    {
        return None;
    }
    text.parse().ok()
}

fn parse_bool(text: &str, true_values: &[String], false_values: &[String]) -> Option<bool> {
    if true_values.iter().any(|value| value == text) {
        return Some(true);
    }
    if false_values.iter().any(|value| value == text) {
        return Some(false);
    }
    match text {
        "True" | "TRUE" | "true" => Some(true),
        "False" | "FALSE" | "false" => Some(false),
        _ => None,
    }
}

/// The values of a column whose type pandas infers: integers without missing values,
/// numbers as floats, booleans, or else texts. Missing values are `nan`.
pub(super) fn infer(
    fields: Vec<Option<String>>,
    true_values: &[String],
    false_values: &[String],
) -> Vec<Value> {
    let present = || fields.iter().flatten();
    let any_missing = fields.iter().any(Option::is_none);
    if present().next().is_none() {
        return fields.iter().map(|_| nan()).collect();
    }
    let missing_or = |value: &dyn Fn(&str) -> Value| -> Vec<Value> {
        fields
            .iter()
            .map(|field| field.as_deref().map_or_else(nan, value))
            .collect()
    };
    if present().all(|text| parse_int(text).is_some()) {
        if any_missing {
            return missing_or(&|text| Value::Float(parse_int(text).unwrap_or(0) as f64));
        }
        return missing_or(&|text| Value::Int(parse_int(text).unwrap_or(0)));
    }
    if present().all(|text| parse_float(text).is_some()) {
        return missing_or(&|text| Value::Float(parse_float(text).unwrap_or(f64::NAN)));
    }
    if present().all(|text| parse_bool(text, true_values, false_values).is_some()) {
        return missing_or(&|text| {
            Value::Bool(parse_bool(text, true_values, false_values).unwrap_or(false))
        });
    }
    missing_or(&|text| Value::str(text))
}

/// The values of a column read with a type, or inferred without one.
pub(super) fn convert(
    label: &Value,
    fields: Vec<Option<String>>,
    kind: Option<DtypeKind>,
    true_values: &[String],
    false_values: &[String],
) -> Result<Vec<Value>, Error> {
    let cannot = |text: &str, what: &str| {
        Error::value_error(format!(
            "invalid literal for {what} of column {}: '{text}'",
            crate::format::plain_str(label)
        ))
    };
    match kind {
        None | Some(DtypeKind::Category) => Ok(infer(fields, true_values, false_values)),
        Some(DtypeKind::Str | DtypeKind::Object) => Ok(fields
            .into_iter()
            .map(|field| field.map_or_else(nan, Value::str))
            .collect()),
        Some(DtypeKind::Int) => fields
            .into_iter()
            .map(|field| match field {
                None => Err(Error::value_error("Integer column has NA values")),
                Some(text) => parse_int(&text)
                    .map(Value::Int)
                    .ok_or_else(|| cannot(&text, "int")),
            })
            .collect(),
        Some(DtypeKind::NullableInt) => fields
            .into_iter()
            .map(|field| match field {
                None => Ok(nan()),
                Some(text) => parse_int(&text)
                    .map(Value::Int)
                    .ok_or_else(|| cannot(&text, "int")),
            })
            .collect(),
        Some(DtypeKind::Float) => fields
            .into_iter()
            .map(|field| match field {
                None => Ok(nan()),
                Some(text) => parse_float(&text)
                    .map(Value::Float)
                    .ok_or_else(|| cannot(&text, "float")),
            })
            .collect(),
        Some(DtypeKind::Bool) => fields
            .into_iter()
            .map(|field| match field {
                None => Ok(nan()),
                Some(text) => parse_bool(&text, true_values, false_values)
                    .map(Value::Bool)
                    .ok_or_else(|| cannot(&text, "bool")),
            })
            .collect(),
    }
}
