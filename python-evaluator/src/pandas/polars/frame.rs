//! A polars `DataFrame`: named columns, each with its polars data type. It keeps the columns
//! in the table core of the pandas stand-in, whose row labels are only positions here.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::any::Any;
use std::cell::RefCell;

use super::super::frame::FrameData;
use super::super::labels::Labels;
use super::super::method;
use super::dtype::DType;
use super::lazy::LazyFrame;
use super::methods::{TABLE_METHODS, table_method};
use super::series::PolarsSeries;
use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::value::{Key, Native, Value};

pub struct PolarsFrame {
    pub data: RefCell<FrameData>,
    pub dtypes: RefCell<Vec<DType>>,
}

impl PolarsFrame {
    pub fn value(names: Vec<String>, columns: Vec<Vec<Value>>, dtypes: Vec<DType>) -> Value {
        let rows = columns.first().map_or(0, Vec::len);
        let labels = names.into_iter().map(Value::str).collect();
        Value::native(PolarsFrame {
            data: RefCell::new(FrameData::new(
                Labels::single(labels, Value::None),
                columns,
                Labels::range(rows),
            )),
            dtypes: RefCell::new(dtypes),
        })
    }

    pub fn names(&self) -> Vec<String> {
        self.data
            .borrow()
            .columns
            .values
            .iter()
            .map(|label| label.as_str().unwrap_or_default().to_owned())
            .collect()
    }

    fn columns(&self) -> Vec<Vec<Value>> {
        self.data.borrow().data.clone()
    }

    /// A copy that shares nothing with this table.
    pub fn copy(&self) -> Value {
        PolarsFrame::value(self.names(), self.columns(), self.dtypes.borrow().clone())
    }

    /// Each row as a dict by column name.
    pub fn records(&self) -> Vec<Vec<(String, Value)>> {
        let names = self.names();
        let data = self.data.borrow();
        (0..data.rows())
            .map(|row| {
                names
                    .iter()
                    .zip(data.data.iter())
                    .map(|(name, column)| (name.clone(), column[row].clone()))
                    .collect()
            })
            .collect()
    }

    /// Sets a column, adding it at the end when the table has none of that name.
    pub fn set_column(&self, name: &str, values: Vec<Value>) {
        let dtype = DType::of(&values);
        let mut data = self.data.borrow_mut();
        let position = data.column_position(&Value::str(name));
        data.set_column(Value::str(name), values);
        let mut dtypes = self.dtypes.borrow_mut();
        match position {
            Some(position) => dtypes[position] = dtype,
            None => dtypes.push(dtype),
        }
    }

    /// A table of some rows, in the given order.
    pub fn take_rows(&self, rows: &[usize]) -> Value {
        let columns = self
            .columns()
            .iter()
            .map(|column| rows.iter().map(|row| column[*row].clone()).collect())
            .collect();
        PolarsFrame::value(self.names(), columns, self.dtypes.borrow().clone())
    }

    /// `df[row, "col"]`, `df["col"]` or `df[row]`.
    fn item(&self, interpreter: &mut Interpreter, key: &Value) -> Result<Value, Error> {
        let data = self.data.borrow();
        let column_of = |interpreter: &mut Interpreter, key: &Value| -> Result<usize, Error> {
            match key.plain() {
                Value::Int(position) => usize::try_from(*position)
                    .ok()
                    .filter(|position| *position < data.columns.len())
                    .ok_or_else(|| Error::index_error("column index out of range")),
                _ => {
                    let name = interpreter.to_str(key)?;
                    data.column_position(&Value::str(name.as_str()))
                        .ok_or_else(|| {
                            super::error("ColumnNotFoundError", format!("\"{name}\" not found"))
                        })
                }
            }
        };
        let row_of = |position: i64| -> Result<usize, Error> {
            let rows = data.rows() as i64;
            let row = if position < 0 {
                rows + position
            } else {
                position
            };
            usize::try_from(row)
                .ok()
                .filter(|row| (*row as i64) < rows)
                .ok_or_else(|| Error::index_error(format!("index {position} is out of bounds")))
        };
        match key.plain() {
            Value::Tuple(parts) if parts.len() == 2 => {
                let row = row_of(parts[0].as_int().unwrap_or(0))?;
                let column = column_of(interpreter, &parts[1])?;
                Ok(data.data[column][row].clone())
            }
            Value::Int(position) => {
                let row = row_of(*position)?;
                drop(data);
                Ok(self.take_rows(&[row]))
            }
            _ => {
                let column = column_of(interpreter, key)?;
                Ok(PolarsSeries::value(
                    data.columns.values[column].as_str().unwrap_or_default(),
                    data.data[column].clone(),
                ))
            }
        }
    }
}

impl Native for PolarsFrame {
    fn type_name(&self) -> &str {
        "DataFrame"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        this: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        let rows = self.data.borrow().rows();
        let names = self.names();
        Ok(Some(match name {
            "columns" => Value::list(names.into_iter().map(Value::str).collect()),
            "height" => Value::Int(rows as i64),
            "width" => Value::Int(names.len() as i64),
            "shape" => Value::tuple(vec![
                Value::Int(rows as i64),
                Value::Int(names.len() as i64),
            ]),
            "dtypes" => Value::list(
                self.dtypes
                    .borrow()
                    .iter()
                    .map(|dtype| Value::native(*dtype))
                    .collect(),
            ),
            "lazy" => {
                let this = this.clone();
                Value::builtin("lazy", move |_, _| Ok(LazyFrame::value(this.clone())))
            }
            "iter_rows" | "rows" | "to_dicts" => {
                let which = name.to_owned();
                method::<PolarsFrame>(this, name, move |interpreter, frame, mut args| {
                    let named = which == "to_dicts"
                        || match args.take_keyword("named") {
                            Some(value) => interpreter.truthy(&value)?,
                            None => false,
                        };
                    let rows = frame.records().into_iter().map(|record| {
                        if named {
                            Value::dict_of(record)
                        } else {
                            Value::tuple(record.into_iter().map(|(_, value)| value).collect())
                        }
                    });
                    let rows: Vec<Value> = rows.collect();
                    Ok(if which == "iter_rows" {
                        Value::iterator(rows)
                    } else {
                        Value::list(rows)
                    })
                })
            }
            "get_column" => method::<PolarsFrame>(this, name, |interpreter, frame, args| {
                frame.item(interpreter, args.get(0).unwrap_or(&Value::None))
            }),
            _ if TABLE_METHODS.contains(&name) => {
                let which = name.to_owned();
                method::<PolarsFrame>(this, name, move |interpreter, frame, args| {
                    table_method(interpreter, frame, &which, args)
                })
            }
            _ => return Ok(None),
        }))
    }

    fn get_item(
        &self,
        interpreter: &mut Interpreter,
        _: &Value,
        key: &Value,
    ) -> Result<Option<Value>, Error> {
        self.item(interpreter, key).map(Some)
    }

    fn length(&self) -> Option<usize> {
        Some(self.data.borrow().rows())
    }
}

/// The column names of dict records, in the order they first appear, as polars builds a
/// table from rows.
pub fn record_names(
    interpreter: &mut Interpreter,
    records: &[Value],
) -> Result<Vec<String>, Error> {
    let mut names: Vec<String> = Vec::new();
    for record in records {
        for (key, _) in interpreter.mapping_items(record)? {
            let key = interpreter.to_str(&key)?;
            if !names.contains(&key) {
                names.push(key);
            }
        }
    }
    Ok(names)
}

/// A value of a dict record, or null.
pub fn record_value(record: &Value, name: &str) -> Value {
    match record.plain() {
        Value::Dict(dict) => dict
            .borrow()
            .entries
            .get(&Key::str(name))
            .cloned()
            .unwrap_or(Value::None),
        _ => Value::None,
    }
}
