//! The one table of nf-r-ipc's Arrow IPC streams: a control row, then one row per value node,
//! read by column name and written with the column types of the request.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;
use std::sync::Arc;

use arrow_array::cast::AsArray;
use arrow_array::types::{Float32Type, Float64Type, Int8Type, Int16Type, Int32Type, Int64Type};
use arrow_array::{
    Array, ArrayRef, BooleanArray, Float64Array, Int32Array, Int64Array, RecordBatch, StringArray,
};
use arrow_ipc::reader::StreamReader;
use arrow_ipc::writer::StreamWriter;
use arrow_schema::{DataType, Field, Schema};

/// The `section` of the control row and of the value rows.
pub(crate) const CONTROL: &str = "__nfr_control__";
pub(crate) const DATA: &str = "__nfr_data__";

/// One row of the table; control rows set the control fields, value rows the node fields.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Row {
    pub section: Option<String>,
    pub protocol: Option<i64>,
    /// The texts of the control fields, in [`CONTROL_FIELDS`] order.
    pub control: [Option<String>; 9],
    pub id: Option<i64>,
    pub parent: Option<i64>,
    pub key: Option<String>,
    pub index: Option<i64>,
    pub tag: Option<String>,
    pub text: Option<String>,
    pub integer: Option<i64>,
    pub float: Option<f64>,
    pub boolean: Option<bool>,
}

/// The text columns of the control row, in table order.
pub(crate) const CONTROL_FIELDS: [&str; 9] = [
    "call_id",
    "function",
    "script_mode",
    "script_ref",
    "payload_kind",
    "status",
    "result_kind",
    "error_class",
    "error_message",
];

/// Writes the rows as one record batch of an IPC stream.
pub(crate) fn write(rows: &[Row], path: &Path) -> Result<(), String> {
    let text = |name: &str| Field::new(name, DataType::Utf8, true);
    let mut fields = vec![
        text("section"),
        Field::new("protocol_version", DataType::Int32, true),
    ];
    fields.extend(CONTROL_FIELDS.iter().map(|name| text(name)));
    fields.extend([
        Field::new("value_id", DataType::Int64, true),
        Field::new("parent_id", DataType::Int64, true),
        text("key"),
        Field::new("index", DataType::Int32, true),
        text("tag"),
        text("v_string"),
        Field::new("v_int64", DataType::Int64, true),
        Field::new("v_float64", DataType::Float64, true),
        Field::new("v_bool", DataType::Boolean, true),
    ]);
    let schema = Arc::new(Schema::new(fields));
    let texts = |pick: &dyn Fn(&Row) -> Option<&str>| -> ArrayRef {
        Arc::new(rows.iter().map(pick).collect::<StringArray>())
    };
    let small = |pick: &dyn Fn(&Row) -> Option<i64>| -> ArrayRef {
        let values = rows
            .iter()
            .map(|row| pick(row).and_then(|value| i32::try_from(value).ok()));
        Arc::new(values.collect::<Int32Array>())
    };
    let large = |pick: &dyn Fn(&Row) -> Option<i64>| -> ArrayRef {
        Arc::new(rows.iter().map(pick).collect::<Int64Array>())
    };
    let mut columns = vec![
        texts(&|row| row.section.as_deref()),
        small(&|row| row.protocol),
    ];
    for place in 0..CONTROL_FIELDS.len() {
        columns.push(texts(&|row| row.control[place].as_deref()));
    }
    columns.extend([
        large(&|row| row.id),
        large(&|row| row.parent),
        texts(&|row| row.key.as_deref()),
        small(&|row| row.index),
        texts(&|row| row.tag.as_deref()),
        texts(&|row| row.text.as_deref()),
        large(&|row| row.integer),
        Arc::new(rows.iter().map(|row| row.float).collect::<Float64Array>()),
        Arc::new(rows.iter().map(|row| row.boolean).collect::<BooleanArray>()),
    ]);
    let batch = RecordBatch::try_new(schema.clone(), columns).map_err(|error| error.to_string())?;
    let file = std::fs::File::create(path)
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    let mut writer = StreamWriter::try_new(file, &schema).map_err(|error| error.to_string())?;
    writer.write(&batch).map_err(|error| error.to_string())?;
    writer.finish().map_err(|error| error.to_string())
}

/// The rows of every batch of an IPC stream; columns are found by name, and integer columns
/// may be integers or doubles.
pub(crate) fn read(path: &Path) -> Result<Vec<Row>, String> {
    let file = std::fs::File::open(path)
        .map_err(|_| format!("IPC response file not found: {}", path.display()))?;
    let reader = StreamReader::try_new(file, None).map_err(|error| error.to_string())?;
    let mut rows = Vec::new();
    let mut batches = 0;
    for batch in reader {
        let batch = batch.map_err(|error| error.to_string())?;
        batches += 1;
        let column = |name: &str| batch.column_by_name(name).cloned();
        let (section, protocol) = (column("section"), column("protocol_version"));
        let controls: Vec<Option<ArrayRef>> =
            CONTROL_FIELDS.iter().map(|name| column(name)).collect();
        let (id, parent, key) = (column("value_id"), column("parent_id"), column("key"));
        let (index, tag, text) = (column("index"), column("tag"), column("v_string"));
        let (integer, float) = (column("v_int64"), column("v_float64"));
        let boolean = column("v_bool");
        for place in 0..batch.num_rows() {
            let mut control: [Option<String>; 9] = Default::default();
            for (slot, array) in control.iter_mut().zip(&controls) {
                *slot = text_at(array.as_ref(), place);
            }
            rows.push(Row {
                section: text_at(section.as_ref(), place),
                protocol: integer_at(protocol.as_ref(), place),
                control,
                id: integer_at(id.as_ref(), place),
                parent: integer_at(parent.as_ref(), place),
                key: text_at(key.as_ref(), place),
                index: integer_at(index.as_ref(), place),
                tag: text_at(tag.as_ref(), place),
                text: text_at(text.as_ref(), place),
                integer: integer_at(integer.as_ref(), place),
                float: number_at(float.as_ref(), place),
                boolean: boolean_at(boolean.as_ref(), place),
            });
        }
    }
    if batches == 0 {
        return Err("IPC stream has no batches".to_owned());
    }
    Ok(rows)
}

fn text_at(array: Option<&ArrayRef>, place: usize) -> Option<String> {
    let array = array?;
    if array.is_null(place) {
        return None;
    }
    match array.data_type() {
        DataType::Utf8 => Some(array.as_string::<i32>().value(place).to_owned()),
        DataType::LargeUtf8 => Some(array.as_string::<i64>().value(place).to_owned()),
        DataType::Utf8View => Some(array.as_string_view().value(place).to_owned()),
        _ => None,
    }
}

/// A number of an integer or floating point column.
fn number_at(array: Option<&ArrayRef>, place: usize) -> Option<f64> {
    let array = array?;
    if array.is_null(place) {
        return None;
    }
    Some(match array.data_type() {
        DataType::Int8 => f64::from(array.as_primitive::<Int8Type>().value(place)),
        DataType::Int16 => f64::from(array.as_primitive::<Int16Type>().value(place)),
        DataType::Int32 => f64::from(array.as_primitive::<Int32Type>().value(place)),
        DataType::Int64 => array.as_primitive::<Int64Type>().value(place) as f64,
        DataType::Float32 => f64::from(array.as_primitive::<Float32Type>().value(place)),
        DataType::Float64 => array.as_primitive::<Float64Type>().value(place),
        _ => return None,
    })
}

/// An integer of an integer column, exactly, or of a floating point column.
fn integer_at(array: Option<&ArrayRef>, place: usize) -> Option<i64> {
    let found = array?;
    match found.data_type() {
        DataType::Int64 if !found.is_null(place) => {
            Some(found.as_primitive::<Int64Type>().value(place))
        }
        _ => number_at(array, place).map(|value| value as i64),
    }
}

/// A boolean of a boolean column, or of a number column, where any number but 0 is true.
fn boolean_at(array: Option<&ArrayRef>, place: usize) -> Option<bool> {
    let found = array?;
    match found.data_type() {
        DataType::Boolean if !found.is_null(place) => Some(found.as_boolean().value(place)),
        _ => number_at(array, place).map(|value| value != 0.0),
    }
}
