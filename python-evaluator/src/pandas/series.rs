//! `pandas.Series`: one column of values with row labels and a name.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::any::Any;
use std::cell::RefCell;

use super::indexer::select;
use super::labels::Labels;
use crate::error::Error;
use crate::interpreter::{Interpreter, Operator};
use crate::value::{Native, Value};

#[derive(Clone)]
pub struct SeriesData {
    pub values: Vec<Value>,
    pub index: Labels,
    pub name: Value,
}

impl SeriesData {
    pub fn take(&self, positions: &[usize]) -> SeriesData {
        SeriesData {
            values: positions
                .iter()
                .map(|position| self.values[*position].clone())
                .collect(),
            index: self.index.take(positions),
            name: self.name.clone(),
        }
    }

    /// The same labels and name with other values.
    pub fn with_values(&self, values: Vec<Value>) -> SeriesData {
        SeriesData {
            values,
            index: self.index.clone(),
            name: self.name.clone(),
        }
    }
}

pub struct Series {
    pub data: RefCell<SeriesData>,
}

impl Series {
    pub fn value(values: Vec<Value>, index: Labels, name: Value) -> Value {
        Series::from_data(SeriesData {
            values,
            index,
            name,
        })
    }

    pub fn from_data(data: SeriesData) -> Value {
        Value::native(Series {
            data: RefCell::new(data),
        })
    }

    pub fn data(&self) -> SeriesData {
        self.data.borrow().clone()
    }
}

impl Native for Series {
    fn type_name(&self) -> &str {
        "Series"
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
        Ok(super::series_methods::attribute(this, self.data(), name))
    }

    fn set_attribute(&self, _: &mut Interpreter, name: &str, value: Value) -> Result<bool, Error> {
        match name {
            "name" => self.data.borrow_mut().name = value,
            _ => return Ok(false),
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
        // A mask or a slice of positions selects by position.
        if let Some(positions) = super::mask_or_slice(interpreter, key, &data.index)? {
            return Ok(Some(Series::from_data(data.take(&positions))));
        }
        match select(interpreter, &data.index, key) {
            Ok(selection) if selection.scalar => {
                Ok(Some(data.values[selection.positions[0]].clone()))
            }
            Ok(selection) => {
                let mut taken = data.take(&selection.positions);
                taken.index = selection.labels;
                Ok(Some(Series::from_data(taken)))
            }
            // An integer key of labels that are not integers counts as a position, as in
            // pandas before 3.0.
            Err(error) if error.is("KeyError") && !data.index.is_integer() => match key.plain() {
                Value::Int(_) => {
                    let position = crate::interpreter::call::normalize_index(
                        key,
                        data.values.len() as i64,
                        "index",
                    )?;
                    Ok(Some(data.values[position].clone()))
                }
                _ => Err(error),
            },
            Err(error) => Err(error),
        }
    }

    fn set_item(
        &self,
        interpreter: &mut Interpreter,
        key: &Value,
        value: Value,
    ) -> Result<bool, Error> {
        let index = self.data.borrow().index.clone();
        let positions = match super::mask_or_slice(interpreter, key, &index)? {
            Some(positions) => positions,
            None => {
                let index = self.data.borrow().index.clone();
                index.find(key)
            }
        };
        let mut data = self.data.borrow_mut();
        if positions.is_empty()
            && !super::is_list_like(key)
            && !matches!(key.plain(), Value::Slice(_))
        {
            data.values.push(value);
            data.index.values.push(key.clone());
            return Ok(true);
        }
        for position in positions {
            data.values[position] = value.clone();
        }
        Ok(true)
    }

    fn iterate(&self, _: &mut Interpreter) -> Result<Option<Vec<Value>>, Error> {
        Ok(Some(self.data.borrow().values.clone()))
    }

    fn length(&self) -> Option<usize> {
        Some(self.data.borrow().values.len())
    }

    fn contains(&self, _: &mut Interpreter, item: &Value) -> Result<Option<bool>, Error> {
        Ok(Some(!self.data.borrow().index.find(item).is_empty()))
    }

    fn hashable(&self) -> bool {
        false
    }

    fn binary(
        &self,
        interpreter: &mut Interpreter,
        operator: Operator,
        other: &Value,
        reflected: bool,
    ) -> Result<Option<Value>, Error> {
        let data = self.data();
        if let Some(series) = other.as_native::<Series>() {
            let other = series.data();
            let identical = data.index.len() == other.index.len()
                && data
                    .index
                    .values
                    .iter()
                    .zip(&other.index.values)
                    .all(|(a, b)| super::same_label(a, b));
            if !identical
                && matches!(
                    operator,
                    Operator::Eq
                        | Operator::NotEq
                        | Operator::Lt
                        | Operator::LtE
                        | Operator::Gt
                        | Operator::GtE
                )
            {
                return Err(Error::value_error(
                    "Can only compare identically-labeled Series objects",
                ));
            }
            if !identical {
                let unique = |index: &Labels| {
                    index.values.iter().enumerate().all(|(position, label)| {
                        !index.values[..position]
                            .iter()
                            .any(|known| super::same_label(known, label))
                    })
                };
                if !unique(&data.index) || !unique(&other.index) {
                    return Err(Error::unsupported(
                        "Series arithmetic reindexing with duplicate labels",
                    ));
                }
                let mut labels = data.index.values.clone();
                for label in &other.index.values {
                    if !labels.iter().any(|known| super::same_label(known, label)) {
                        labels.push(label.clone());
                    }
                }
                let (missing, present): (Vec<Value>, Vec<Value>) =
                    labels.into_iter().partition(super::is_missing);
                let mut labels = crate::builtins::sort_values(interpreter, present, None, false)?;
                labels.extend(missing);
                let aligned = |source: &SeriesData| -> Vec<Value> {
                    labels
                        .iter()
                        .map(|label| {
                            source
                                .index
                                .values
                                .iter()
                                .position(|known| super::same_label(known, label))
                                .map_or_else(super::nan, |position| source.values[position].clone())
                        })
                        .collect()
                };
                let values = super::elementwise(
                    interpreter,
                    operator,
                    &aligned(&data),
                    &Value::list(aligned(&other)),
                    reflected,
                )?;
                return Ok(values.map(|values| {
                    Series::value(
                        values,
                        Labels {
                            values: labels,
                            names: data.index.names.clone(),
                        },
                        data.name.clone(),
                    )
                }));
            }
        }
        let result = super::elementwise(interpreter, operator, &data.values, other, reflected)?;
        Ok(result.map(|values| Series::from_data(data.with_values(values))))
    }

    fn unary(&self, interpreter: &mut Interpreter, operator: &str) -> Result<Option<Value>, Error> {
        let data = self.data();
        let result = super::unary_values(interpreter, operator, &data.values)?;
        Ok(result.map(|values| Series::from_data(data.with_values(values))))
    }

    fn to_str(&self, interpreter: &mut Interpreter) -> Result<Option<String>, Error> {
        self.repr(interpreter).map(Some)
    }

    fn repr(&self, interpreter: &mut Interpreter) -> Result<String, Error> {
        super::display::series_text(interpreter, &self.data())
    }
}
