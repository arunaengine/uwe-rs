//! `pandas.DataFrame`: columns of values that share row labels. The methods are in
//! `frame_methods`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;

use super::array::Array;
use super::frame_methods as methods;
use super::index::IndexObject;
use super::indexer::Indexer;
use super::labels::{Labels, labels_of};
use super::method;
use super::reshape;
use super::series::{Series, SeriesData};
use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::value::{Args, Native, Value, key_equal};

/// The columns of a table. The row labels live behind a shared cell, so that `df.index`
/// can rename their levels.
#[derive(Clone)]
pub struct FrameData {
    pub columns: Labels,
    pub data: Vec<Vec<Value>>,
    pub index: Rc<RefCell<Labels>>,
}

impl FrameData {
    pub fn new(columns: Labels, data: Vec<Vec<Value>>, index: Labels) -> FrameData {
        FrameData {
            columns,
            data,
            index: Rc::new(RefCell::new(index)),
        }
    }

    /// A copy that shares nothing with this table.
    pub fn copy(&self) -> FrameData {
        FrameData::new(self.columns.clone(), self.data.clone(), self.index())
    }

    pub fn index(&self) -> Labels {
        self.index.borrow().clone()
    }

    pub fn rows(&self) -> usize {
        self.index.borrow().len()
    }

    pub fn column_position(&self, key: &Value) -> Option<usize> {
        self.columns
            .values
            .iter()
            .position(|label| key_equal(label, key))
    }

    pub fn column(&self, position: usize) -> SeriesData {
        SeriesData {
            values: self.data[position].clone(),
            index: self.index(),
            name: self.columns.values[position].clone(),
        }
    }

    /// One row over the given columns, named by its label.
    pub fn row(&self, position: usize, columns: &[usize]) -> SeriesData {
        SeriesData {
            values: columns
                .iter()
                .map(|column| self.data[*column][position].clone())
                .collect(),
            index: self.columns.take(columns),
            name: self.index.borrow().values[position].clone(),
        }
    }

    pub fn take_rows(&self, positions: &[usize]) -> FrameData {
        FrameData::new(
            self.columns.clone(),
            self.data
                .iter()
                .map(|column| positions.iter().map(|row| column[*row].clone()).collect())
                .collect(),
            self.index().take(positions),
        )
    }

    pub fn take_columns(&self, positions: &[usize]) -> FrameData {
        FrameData::new(
            self.columns.take(positions),
            positions
                .iter()
                .map(|position| self.data[*position].clone())
                .collect(),
            self.index(),
        )
    }

    /// Sets a column, adding it at the end when the table has none of that label.
    pub fn set_column(&mut self, label: Value, values: Vec<Value>) {
        match self.column_position(&label) {
            Some(position) => self.data[position] = values,
            None => {
                self.columns.values.push(label);
                self.data.push(values);
            }
        }
    }

    pub fn all_columns(&self) -> Vec<usize> {
        (0..self.columns.len()).collect()
    }
}

pub struct DataFrame {
    pub data: RefCell<FrameData>,
}

impl DataFrame {
    pub fn from_data(data: FrameData) -> Value {
        Value::native(DataFrame {
            data: RefCell::new(data),
        })
    }

    pub fn data(&self) -> FrameData {
        self.data.borrow().clone()
    }

    /// The values for a new column: a series by its labels, a list or array as it is, or one
    /// value for every row.
    pub fn column_values(
        &self,
        interpreter: &mut Interpreter,
        value: &Value,
    ) -> Result<Vec<Value>, Error> {
        let data = self.data();
        methods::column_values(interpreter, &data, value)
    }
}

pub(super) fn frame_method(
    this: &Value,
    name: &str,
    function: fn(&mut Interpreter, FrameData, Args) -> Result<Value, Error>,
) -> Value {
    method::<DataFrame>(this, name, move |interpreter, frame, args| {
        function(interpreter, frame.data(), args)
    })
}

/// A method that may change the table in place with `inplace=True`.
fn inplace_method(
    this: &Value,
    name: &str,
    function: fn(&mut Interpreter, FrameData, &mut Args) -> Result<FrameData, Error>,
) -> Value {
    method::<DataFrame>(this, name, move |interpreter, frame, mut args| {
        let inplace = match args.take_keyword("inplace") {
            Some(value) => interpreter.truthy(&value)?,
            None => false,
        };
        let result = function(interpreter, frame.data().copy(), &mut args)?;
        if inplace {
            *frame.data.borrow_mut() = result;
            return Ok(Value::None);
        }
        Ok(DataFrame::from_data(result))
    })
}

impl Native for DataFrame {
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
        let data = self.data();
        Ok(Some(match name {
            "columns" => IndexObject::value(data.columns.clone()),
            "index" => Value::native(IndexObject {
                labels: data.index.clone(),
            }),
            "shape" => Value::tuple(vec![
                Value::Int(data.rows() as i64),
                Value::Int(data.columns.len() as i64),
            ]),
            "size" => Value::Int((data.rows() * data.columns.len()) as i64),
            "ndim" => Value::Int(2),
            "empty" => Value::Bool(data.rows() == 0 || data.columns.is_empty()),
            "values" => Array::value(
                (0..data.rows())
                    .map(|row| Array::value(data.row(row, &data.all_columns()).values))
                    .collect(),
            ),
            "dtypes" => Series::value(
                data.data
                    .iter()
                    .map(|column| Value::str(super::dtype_name(column)))
                    .collect(),
                data.columns.clone(),
                Value::None,
            ),
            "T" => DataFrame::from_data(super::reshape::transpose(&data)),
            "loc" | "iloc" | "at" | "iat" => Indexer::value(this, name),
            "set_index" => inplace_method(this, name, reshape::set_index),
            "reset_index" => inplace_method(this, name, reshape::reset_index),
            "sort_index" => inplace_method(this, name, reshape::sort_index),
            "sort_values" => inplace_method(this, name, reshape::sort_values),
            "dropna" => inplace_method(this, name, reshape::dropna),
            "fillna" => inplace_method(this, name, reshape::fillna),
            "drop_duplicates" => inplace_method(this, name, reshape::drop_duplicates),
            "drop" => inplace_method(this, name, reshape::drop),
            "rename" => inplace_method(this, name, reshape::rename),
            "astype" => frame_method(this, name, methods::astype),
            "duplicated" => frame_method(this, name, reshape::duplicated),
            "copy" => frame_method(this, name, |_, data, _| {
                Ok(DataFrame::from_data(data.copy()))
            }),
            "head" | "tail" => {
                let head = name == "head";
                method::<DataFrame>(this, name, move |_, frame, args| {
                    let data = frame.data();
                    let count = args.get(0).and_then(Value::as_int).unwrap_or(5).max(0) as usize;
                    let rows = data.rows();
                    let positions: Vec<usize> = if head {
                        (0..count.min(rows)).collect()
                    } else {
                        (rows.saturating_sub(count)..rows).collect()
                    };
                    Ok(DataFrame::from_data(data.take_rows(&positions)))
                })
            }
            "isin" => frame_method(this, name, methods::isin),
            "isna" | "isnull" => frame_method(this, name, |_, data, _| {
                Ok(methods::map_cells(&data, |value| {
                    Value::Bool(super::is_missing(value))
                }))
            }),
            "notna" | "notnull" => frame_method(this, name, |_, data, _| {
                Ok(methods::map_cells(&data, |value| {
                    Value::Bool(!super::is_missing(value))
                }))
            }),
            "apply" => frame_method(this, name, methods::apply),
            "itertuples" => frame_method(this, name, methods::itertuples),
            "iterrows" => frame_method(this, name, |_, data, _| {
                let columns = data.all_columns();
                Ok(Value::iterator((0..data.rows()).map(|row| {
                    let series = data.row(row, &columns);
                    Value::tuple(vec![series.name.clone(), Series::from_data(series)])
                })))
            }),
            "items" | "iteritems" => frame_method(this, name, |_, data, _| {
                Ok(Value::iterator((0..data.columns.len()).map(|position| {
                    Value::tuple(vec![
                        data.columns.values[position].clone(),
                        Series::from_data(data.column(position)),
                    ])
                })))
            }),
            "keys" => frame_method(this, name, |_, data, _| {
                Ok(IndexObject::value(data.columns))
            }),
            "to_dict" => frame_method(this, name, methods::to_dict),
            "get" => frame_method(this, name, |_, data, mut args| {
                let key = args.take(0, "key").unwrap_or(Value::None);
                let default = args.take(1, "default").unwrap_or(Value::None);
                Ok(match data.column_position(&key) {
                    Some(position) => Series::from_data(data.column(position)),
                    None => default,
                })
            }),
            "assign" => frame_method(this, name, methods::assign),
            "filter" => frame_method(this, name, methods::filter),
            "merge" => frame_method(this, name, |interpreter, data, mut args| {
                let right = args
                    .take(0, "right")
                    .ok_or_else(|| Error::type_error("merge() missing right"))?;
                super::combine::merge(interpreter, DataFrame::from_data(data), right, args)
            }),
            "update" => method::<DataFrame>(this, name, methods::update),
            "insert" => method::<DataFrame>(this, name, methods::insert),
            "pop" => method::<DataFrame>(this, name, |_, frame, args| {
                let key = args.get(0).cloned().unwrap_or(Value::None);
                let mut data = frame.data.borrow_mut();
                let position = data
                    .column_position(&key)
                    .ok_or_else(|| Error::key_error(key.clone()))?;
                let series = data.column(position);
                data.columns.values.remove(position);
                data.data.remove(position);
                Ok(Series::from_data(series))
            }),
            "to_csv" => frame_method(this, name, methods::to_csv),
            "nunique" | "count" | "any" | "all" => {
                let which = name.to_owned();
                method::<DataFrame>(this, name, move |interpreter, frame, _| {
                    methods::reduce_columns(interpreter, &frame.data(), &which)
                })
            }
            "equals" => frame_method(this, name, |_, data, args| {
                let Some(other) = args.get(0).and_then(|other| other.as_native::<DataFrame>())
                else {
                    return Ok(Value::Bool(false));
                };
                let other = other.data();
                Ok(Value::Bool(
                    other.columns.values.len() == data.columns.values.len()
                        && other.data.iter().zip(data.data.iter()).all(|(a, b)| {
                            a.len() == b.len()
                                && a.iter().zip(b).all(|(x, y)| super::same_label(x, y))
                        }),
                ))
            }),
            "transpose" => frame_method(this, name, |_, data, _| {
                Ok(DataFrame::from_data(super::reshape::transpose(&data)))
            }),
            "groupby" => frame_method(this, name, super::group::groupby),
            "query" => frame_method(this, name, |interpreter, data, mut args| {
                let expression = args.take(0, "expr").unwrap_or(Value::None);
                let expression = interpreter.to_str(&expression)?;
                Ok(DataFrame::from_data(super::query::query(
                    interpreter,
                    &data,
                    &expression,
                )?))
            }),
            _ => match data.column_position(&Value::str(name)) {
                Some(position) => Series::from_data(data.column(position)),
                None => return Ok(None),
            },
        }))
    }

    fn set_attribute(
        &self,
        interpreter: &mut Interpreter,
        name: &str,
        value: Value,
    ) -> Result<bool, Error> {
        match name {
            "columns" => {
                let labels = labels_of(interpreter, &value)?;
                let mut data = self.data.borrow_mut();
                if labels.len() != data.columns.len() {
                    return Err(Error::value_error(format!(
                        "Length mismatch: Expected axis has {} elements, new values have {} elements",
                        data.columns.len(),
                        labels.len()
                    )));
                }
                data.columns = labels;
            }
            "index" => {
                let labels = labels_of(interpreter, &value)?;
                let mut data = self.data.borrow_mut();
                if labels.len() != data.rows() {
                    return Err(Error::value_error(format!(
                        "Length mismatch: Expected axis has {} elements, new values have {} elements",
                        data.rows(),
                        labels.len()
                    )));
                }
                data.index = Rc::new(RefCell::new(labels));
            }
            _ => {
                let values = self.column_values(interpreter, &value)?;
                self.data.borrow_mut().set_column(Value::str(name), values);
            }
        }
        Ok(true)
    }

    fn get_item(
        &self,
        interpreter: &mut Interpreter,
        _: &Value,
        key: &Value,
    ) -> Result<Option<Value>, Error> {
        let data = self.data();
        if let Some(positions) = super::mask_or_positional_slice(interpreter, key, data.rows())? {
            return Ok(Some(DataFrame::from_data(data.take_rows(&positions))));
        }
        if super::is_list_like(key) {
            let mut positions = Vec::new();
            for label in super::seq_values(interpreter, key)? {
                positions.push(
                    data.column_position(&label)
                        .ok_or_else(|| Error::key_error(label.clone()))?,
                );
            }
            return Ok(Some(DataFrame::from_data(data.take_columns(&positions))));
        }
        match data.column_position(key) {
            Some(position) => Ok(Some(Series::from_data(data.column(position)))),
            None => Err(Error::key_error(key.clone())),
        }
    }

    fn set_item(
        &self,
        interpreter: &mut Interpreter,
        key: &Value,
        value: Value,
    ) -> Result<bool, Error> {
        if super::is_list_like(key) {
            let labels = super::seq_values(interpreter, key)?;
            for label in labels {
                let values = match value.as_native::<DataFrame>() {
                    Some(other) => {
                        let other = other.data();
                        let position = other
                            .column_position(&label)
                            .ok_or_else(|| Error::key_error(label.clone()))?;
                        other.data[position].clone()
                    }
                    None => self.column_values(interpreter, &value)?,
                };
                self.data.borrow_mut().set_column(label, values);
            }
            return Ok(true);
        }
        let values = self.column_values(interpreter, &value)?;
        self.data.borrow_mut().set_column(key.clone(), values);
        Ok(true)
    }

    fn iterate(&self, _: &mut Interpreter) -> Result<Option<Vec<Value>>, Error> {
        Ok(Some(self.data.borrow().columns.values.clone()))
    }

    fn length(&self) -> Option<usize> {
        Some(self.data.borrow().rows())
    }

    fn contains(&self, _: &mut Interpreter, item: &Value) -> Result<Option<bool>, Error> {
        Ok(Some(self.data.borrow().column_position(item).is_some()))
    }

    fn hashable(&self) -> bool {
        false
    }

    fn to_str(&self, interpreter: &mut Interpreter) -> Result<Option<String>, Error> {
        self.repr(interpreter).map(Some)
    }

    fn repr(&self, interpreter: &mut Interpreter) -> Result<String, Error> {
        super::display::frame_text(interpreter, &self.data())
    }
}
