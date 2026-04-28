//! How series, tables and indexes print, close to pandas' layout.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::frame::FrameData;
use super::is_missing;
use super::labels::Labels;
use super::series::SeriesData;
use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::value::Value;

/// A value as pandas shows it: `NaN` for a missing value, texts plain or quoted.
pub fn value_text(
    interpreter: &mut Interpreter,
    value: &Value,
    quoted: bool,
) -> Result<String, Error> {
    if is_missing(value) {
        return Ok(if value.is_none() {
            "None".into()
        } else {
            "NaN".into()
        });
    }
    if quoted {
        interpreter.repr(value)
    } else {
        interpreter.to_str(value)
    }
}

/// The values as the items of a list shows them.
pub fn text_list(interpreter: &mut Interpreter, values: &[Value]) -> Result<String, Error> {
    let mut items = Vec::with_capacity(values.len());
    for value in values {
        items.push(if is_missing(value) {
            "nan".to_owned()
        } else {
            interpreter.repr(value)?
        });
    }
    Ok(items.join(", "))
}

/// A series: one line per label and value, then its name and type.
pub fn series_text(interpreter: &mut Interpreter, data: &SeriesData) -> Result<String, Error> {
    let mut rows: Vec<(String, String)> = Vec::new();
    for (label, value) in data.index.values.iter().zip(data.values.iter()) {
        rows.push((
            value_text(interpreter, label, false)?,
            value_text(interpreter, value, false)?,
        ));
    }
    let label_width = rows
        .iter()
        .map(|(label, _)| label.chars().count())
        .max()
        .unwrap_or(0);
    let value_width = rows
        .iter()
        .map(|(_, value)| value.chars().count())
        .max()
        .unwrap_or(0);
    let mut text = String::new();
    for (label, value) in &rows {
        text.push_str(&format!("{label:<label_width$}    {value:>value_width$}\n"));
    }
    if !data.name.is_none() {
        text.push_str(&format!(
            "Name: {}, ",
            value_text(interpreter, &data.name, false)?
        ));
    }
    text.push_str(&format!("dtype: {}", super::dtype_name(&data.values)));
    Ok(text)
}

/// A table: a header of column labels, then one line per row.
pub fn frame_text(interpreter: &mut Interpreter, data: &FrameData) -> Result<String, Error> {
    let index = data.index();
    let mut table: Vec<Vec<String>> = Vec::new();
    let mut header = vec![String::new()];
    for label in &data.columns.values {
        header.push(value_text(interpreter, label, false)?);
    }
    table.push(header);
    for row in 0..data.rows() {
        let mut line = vec![value_text(interpreter, &index.values[row], false)?];
        for column in &data.data {
            line.push(value_text(interpreter, &column[row], false)?);
        }
        table.push(line);
    }
    if data.rows() == 0 {
        return Ok(format!(
            "Empty DataFrame\nColumns: [{}]\nIndex: []",
            table[0][1..].join(", ")
        ));
    }
    let widths: Vec<usize> = (0..table[0].len())
        .map(|column| {
            table
                .iter()
                .map(|line| line[column].chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    let lines: Vec<String> = table
        .iter()
        .map(|line| {
            let mut text = format!("{:<width$}", line[0], width = widths[0]);
            for (cell, width) in line.iter().zip(&widths).skip(1) {
                text.push_str(&format!("  {cell:>width$}"));
            }
            text.trim_end().to_owned()
        })
        .collect();
    Ok(lines.join("\n"))
}

/// An index: its labels, type and name.
pub fn index_text(interpreter: &mut Interpreter, labels: &Labels) -> Result<String, Error> {
    if labels.is_multi() {
        let lines: Vec<String> = labels
            .values
            .iter()
            .map(crate::format::plain_repr)
            .collect();
        return Ok(format!(
            "MultiIndex([{}],\n           names={})",
            lines.join(",\n            "),
            crate::format::plain_repr(&Value::list(labels.names.clone()))
        ));
    }
    let items = text_list(interpreter, &labels.values)?;
    let mut text = format!(
        "Index([{items}], dtype='{}'",
        super::dtype_name(&labels.values)
    );
    if !labels.names[0].is_none() {
        text.push_str(&format!(
            ", name={}",
            value_text(interpreter, &labels.names[0], true)?
        ));
    }
    text.push(')');
    Ok(text)
}
