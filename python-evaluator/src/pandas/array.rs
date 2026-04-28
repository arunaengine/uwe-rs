//! A stand-in for the one-dimensional NumPy arrays that pandas returns, like `.values` and
//! `.unique()`: a list of values with element-wise comparisons.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::any::Any;
use std::cell::RefCell;

use super::{is_missing, method, positions_of, text_list};
use crate::error::Error;
use crate::interpreter::{Interpreter, Operator};
use crate::value::{Args, Native, Value, key_equal};

pub struct Array {
    pub values: RefCell<Vec<Value>>,
}

impl Array {
    pub fn value(values: Vec<Value>) -> Value {
        Value::native(Array {
            values: RefCell::new(values),
        })
    }

    fn values(&self) -> Vec<Value> {
        self.values.borrow().clone()
    }
}

fn array_method(
    this: &Value,
    name: &str,
    function: fn(&mut Interpreter, Vec<Value>, Args) -> Result<Value, Error>,
) -> Value {
    method::<Array>(this, name, move |interpreter, array, args| {
        function(interpreter, array.values(), args)
    })
}

impl Native for Array {
    fn type_name(&self) -> &str {
        "ndarray"
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
        let length = self.values.borrow().len() as i64;
        Ok(Some(match name {
            "size" => Value::Int(length),
            "shape" => Value::tuple(vec![Value::Int(length)]),
            "ndim" => Value::Int(1),
            "dtype" => Value::str(super::dtype_name(&self.values())),
            "T" => this.clone(),
            "tolist" => array_method(this, name, |_, values, _| {
                Ok(Value::list(
                    values
                        .into_iter()
                        .map(|value| match value.as_native::<Array>() {
                            Some(inner) => Value::list(inner.values()),
                            None => value,
                        })
                        .collect(),
                ))
            }),
            "flatten" | "ravel" | "copy" => {
                array_method(this, name, |_, values, _| Ok(Array::value(values)))
            }
            "astype" => array_method(this, name, |interpreter, values, args| {
                let kind = args.get(0).cloned().unwrap_or(Value::None);
                Ok(Array::value(super::cast_values(
                    interpreter,
                    &values,
                    &kind,
                )?))
            }),
            "item" => array_method(this, name, |_, values, _| match values.as_slice() {
                [single] => Ok(single.clone()),
                _ => Err(Error::value_error(
                    "can only convert an array of size 1 to a Python scalar",
                )),
            }),
            "any" => array_method(this, name, |interpreter, values, _| {
                for value in &values {
                    if interpreter.truthy(value)? {
                        return Ok(Value::Bool(true));
                    }
                }
                Ok(Value::Bool(false))
            }),
            "all" => array_method(this, name, |interpreter, values, _| {
                for value in &values {
                    if !interpreter.truthy(value)? {
                        return Ok(Value::Bool(false));
                    }
                }
                Ok(Value::Bool(true))
            }),
            "sum" => array_method(this, name, |interpreter, values, _| {
                super::sum(interpreter, &values)
            }),
            "min" | "max" => {
                let largest = name == "max";
                method::<Array>(this, name, move |interpreter, array, _| {
                    super::extreme(interpreter, &array.values(), largest)
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
        let values = self.values();
        if let Value::Int(_) = key.plain() {
            let position =
                crate::interpreter::call::normalize_index(key, values.len() as i64, "index")?;
            return Ok(Some(values[position].clone()));
        }
        let positions = positions_of(interpreter, key, values.len())?;
        Ok(Some(Array::value(
            positions
                .iter()
                .map(|position| values[*position].clone())
                .collect(),
        )))
    }

    fn set_item(&self, _: &mut Interpreter, key: &Value, value: Value) -> Result<bool, Error> {
        let mut values = self.values.borrow_mut();
        let position =
            crate::interpreter::call::normalize_index(key, values.len() as i64, "index")?;
        values[position] = value;
        Ok(true)
    }

    fn iterate(&self, _: &mut Interpreter) -> Result<Option<Vec<Value>>, Error> {
        Ok(Some(self.values()))
    }

    fn length(&self) -> Option<usize> {
        Some(self.values.borrow().len())
    }

    fn contains(&self, _: &mut Interpreter, item: &Value) -> Result<Option<bool>, Error> {
        Ok(Some(
            self.values
                .borrow()
                .iter()
                .any(|value| !is_missing(value) && key_equal(value, item)),
        ))
    }

    fn truthy(&self) -> Option<bool> {
        let values = self.values.borrow();
        match values.as_slice() {
            [] => Some(false),
            [Value::Bool(value)] => Some(*value),
            _ => None,
        }
    }

    fn binary(
        &self,
        interpreter: &mut Interpreter,
        operator: Operator,
        other: &Value,
        reflected: bool,
    ) -> Result<Option<Value>, Error> {
        let values = self.values();
        let result = super::elementwise(interpreter, operator, &values, other, reflected)?;
        Ok(result.map(Array::value))
    }

    fn unary(&self, interpreter: &mut Interpreter, operator: &str) -> Result<Option<Value>, Error> {
        let values = self.values();
        Ok(super::unary_values(interpreter, operator, &values)?.map(Array::value))
    }

    fn hashable(&self) -> bool {
        false
    }

    fn repr(&self, interpreter: &mut Interpreter) -> Result<String, Error> {
        let values = self.values();
        let items = text_list(interpreter, &values)?;
        Ok(match super::dtype_name(&values) {
            "int64" | "float64" | "bool" => format!("array([{items}])"),
            _ => format!("array([{items}], dtype=object)"),
        })
    }
}
