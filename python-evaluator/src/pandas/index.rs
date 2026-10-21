//! `pandas.Index` and `pandas.MultiIndex`: the objects that show the labels of a table.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;

use super::array::Array;
use super::labels::Labels;
use super::{method, seq_values};
use crate::error::Error;
use crate::interpreter::{Interpreter, Operator};
use crate::value::{Args, Native, Value, key_equal};

/// `pandas.Index` and `pandas.MultiIndex`. A table shares its labels with the index object,
/// so that setting the names of `df.index` renames the table's levels.
pub struct IndexObject {
    pub labels: Rc<RefCell<Labels>>,
}

impl IndexObject {
    pub fn value(labels: Labels) -> Value {
        Value::native(IndexObject {
            labels: Rc::new(RefCell::new(labels)),
        })
    }

    pub fn labels(&self) -> Labels {
        self.labels.borrow().clone()
    }
}

fn index_method(
    this: &Value,
    name: &str,
    function: fn(&mut Interpreter, Labels, Args) -> Result<Value, Error>,
) -> Value {
    method::<IndexObject>(this, name, move |interpreter, index, args| {
        function(interpreter, index.labels(), args)
    })
}

impl Native for IndexObject {
    fn type_name(&self) -> &str {
        if self.labels.borrow().is_multi() {
            "MultiIndex"
        } else {
            "Index"
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get_attribute(
        &self,
        interpreter: &mut Interpreter,
        this: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        let labels = self.labels();
        Ok(Some(match name {
            "names" => Value::list(labels.names.clone()),
            "name" if labels.is_multi() => Value::None,
            "name" => labels.names[0].clone(),
            "nlevels" => Value::Int(labels.levels() as i64),
            "size" => Value::Int(labels.len() as i64),
            "shape" => Value::tuple(vec![Value::Int(labels.len() as i64)]),
            "empty" => Value::Bool(labels.is_empty()),
            "values" | "array" => Array::value(labels.values.clone()),
            "is_unique" => Value::Bool(labels.is_unique()),
            "has_duplicates" => Value::Bool(!labels.is_unique()),
            "dtype" => Value::str(super::dtype_name(&labels.values)),
            "str" => Value::native(super::strings::StrAccessor(this.clone())),
            "levels" => {
                let levels = labels.sorted_levels(interpreter)?;
                Value::list(
                    levels
                        .into_iter()
                        .zip(labels.names.iter())
                        .map(|(level, name)| {
                            IndexObject::value(Labels::single(level, name.clone()))
                        })
                        .collect(),
                )
            }
            "tolist" | "to_list" => {
                index_method(this, name, |_, labels, _| Ok(Value::list(labels.values)))
            }
            "to_numpy" => index_method(this, name, |_, labels, _| Ok(Array::value(labels.values))),
            "copy" => index_method(this, name, |_, labels, _| Ok(IndexObject::value(labels))),
            "unique" | "drop_duplicates" => index_method(this, name, |_, labels, _| {
                Ok(IndexObject::value(Labels {
                    values: super::distinct(&labels.values),
                    names: labels.names,
                }))
            }),
            "duplicated" => index_method(this, name, |_, labels, _| {
                Ok(Array::value(super::duplicated(&labels.values)))
            }),
            "isin" => index_method(this, name, |interpreter, labels, args| {
                let wanted = seq_values(interpreter, args.get(0).unwrap_or(&Value::None))?;
                Ok(Array::value(
                    labels
                        .values
                        .iter()
                        .map(|label| Value::Bool(wanted.iter().any(|item| key_equal(item, label))))
                        .collect(),
                ))
            }),
            "astype" => index_method(this, name, |interpreter, labels, args| {
                let kind = args.get(0).cloned().unwrap_or(Value::None);
                let values = super::cast_values(interpreter, &labels.values, &kind)?;
                Ok(IndexObject::value(Labels {
                    values,
                    names: labels.names,
                }))
            }),
            "map" => index_method(this, name, |interpreter, labels, args| {
                let function = args.get(0).cloned().unwrap_or(Value::None);
                let values = super::map_values(interpreter, &labels.values, &function)?;
                Ok(IndexObject::value(Labels::single(
                    values,
                    labels.names[0].clone(),
                )))
            }),
            "get_level_values" => index_method(this, name, |_, labels, args| {
                let key = args.get(0).cloned().unwrap_or(Value::Int(0));
                let level = labels
                    .level_of(&key)
                    .ok_or_else(|| Error::key_error(key.clone()))?;
                Ok(IndexObject::value(Labels::single(
                    labels.level(level),
                    labels.names.get(level).cloned().unwrap_or(Value::None),
                )))
            }),
            "droplevel" => index_method(this, name, |_, labels, args| {
                let key = args.get(0).cloned().unwrap_or(Value::Int(0));
                let level = labels
                    .level_of(&key)
                    .ok_or_else(|| Error::key_error(key.clone()))?;
                let keep: Vec<usize> = (0..labels.levels())
                    .filter(|other| *other != level)
                    .collect();
                let levels = keep.iter().map(|level| labels.level(*level)).collect();
                let names = keep
                    .iter()
                    .map(|level| labels.names[*level].clone())
                    .collect();
                Ok(IndexObject::value(Labels::from_levels(levels, names)))
            }),
            "set_levels" => index_method(this, name, set_levels),
            "set_names" | "rename" => index_method(this, name, |interpreter, labels, args| {
                let names = args.get(0).cloned().unwrap_or(Value::None);
                let names = match names.plain() {
                    Value::List(_) | Value::Tuple(_) => seq_values(interpreter, &names)?,
                    _ => vec![names],
                };
                Ok(IndexObject::value(Labels {
                    values: labels.values,
                    names,
                }))
            }),
            "get_loc" => index_method(this, name, |_, labels, args| {
                let key = args.get(0).cloned().unwrap_or(Value::None);
                match labels.find(&key).as_slice() {
                    [] => Err(Error::key_error(key)),
                    [position] => Ok(Value::Int(*position as i64)),
                    positions => Ok(Array::value(
                        (0..labels.len())
                            .map(|position| Value::Bool(positions.contains(&position)))
                            .collect(),
                    )),
                }
            }),
            "difference" | "intersection" | "union" => {
                let operation = name.to_owned();
                let this = this.clone();
                Value::builtin(name, move |interpreter, args| {
                    let Some(index) = this.as_native::<IndexObject>() else {
                        return Err(Error::type_error("not an index"));
                    };
                    let labels = index.labels();
                    let other = seq_values(interpreter, args.get(0).unwrap_or(&Value::None))?;
                    let values: Vec<Value> = match operation.as_str() {
                        "difference" => labels
                            .values
                            .iter()
                            .filter(|label| !other.iter().any(|item| key_equal(item, label)))
                            .cloned()
                            .collect(),
                        "intersection" => labels
                            .values
                            .iter()
                            .filter(|label| other.iter().any(|item| key_equal(item, label)))
                            .cloned()
                            .collect(),
                        _ => labels.values.iter().chain(other.iter()).cloned().collect(),
                    };
                    let values = super::distinct(&values);
                    let values = if operation == "intersection" {
                        values
                    } else {
                        crate::builtins::sort_values(interpreter, values, None, false)?
                    };
                    Ok(IndexObject::value(Labels {
                        values,
                        names: labels.names,
                    }))
                })
            }
            "sort_values" => index_method(this, name, |interpreter, labels, mut args| {
                let ascending = match args.take_keyword("ascending") {
                    Some(value) => interpreter.truthy(&value)?,
                    None => true,
                };
                let values =
                    crate::builtins::sort_values(interpreter, labels.values, None, !ascending)?;
                Ok(IndexObject::value(Labels {
                    values,
                    names: labels.names,
                }))
            }),
            "append" => index_method(this, name, |interpreter, labels, args| {
                let other = seq_values(interpreter, args.get(0).unwrap_or(&Value::None))?;
                let mut values = labels.values;
                values.extend(other);
                Ok(IndexObject::value(Labels {
                    values,
                    names: labels.names,
                }))
            }),
            "to_series" => index_method(this, name, |_, labels, _| {
                let name = labels.names[0].clone();
                Ok(super::series::Series::value(
                    labels.values.clone(),
                    labels,
                    name,
                ))
            }),
            "min" | "max" => {
                let largest = name == "max";
                let this = this.clone();
                Value::builtin(name, move |interpreter, _| {
                    let Some(index) = this.as_native::<IndexObject>() else {
                        return Err(Error::type_error("not an index"));
                    };
                    super::extreme(interpreter, &index.labels().values, largest)
                })
            }
            _ => return Ok(None),
        }))
    }

    fn set_attribute(
        &self,
        interpreter: &mut Interpreter,
        name: &str,
        value: Value,
    ) -> Result<bool, Error> {
        match name {
            "names" => {
                let names = seq_values(interpreter, &value)?;
                let mut labels = self.labels.borrow_mut();
                if names.len() != labels.levels() {
                    return Err(Error::value_error(format!(
                        "Length of names must match number of levels in MultiIndex. Got {} names for {} levels.",
                        names.len(),
                        labels.levels()
                    )));
                }
                labels.names = names;
            }
            "name" => {
                let mut labels = self.labels.borrow_mut();
                labels.names = vec![value];
            }
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
        let labels = self.labels();
        if let Value::Int(_) = key.plain() {
            let position =
                crate::interpreter::call::normalize_index(key, labels.len() as i64, "index")?;
            return Ok(Some(labels.values[position].clone()));
        }
        let positions = super::positions_of(interpreter, key, labels.len())?;
        Ok(Some(IndexObject::value(labels.take(&positions))))
    }

    fn iterate(&self, _: &mut Interpreter) -> Result<Option<Vec<Value>>, Error> {
        Ok(Some(self.labels().values))
    }

    fn length(&self) -> Option<usize> {
        Some(self.labels.borrow().len())
    }

    fn contains(&self, _: &mut Interpreter, item: &Value) -> Result<Option<bool>, Error> {
        Ok(Some(!self.labels.borrow().find(item).is_empty()))
    }

    fn binary(
        &self,
        interpreter: &mut Interpreter,
        operator: Operator,
        other: &Value,
        reflected: bool,
    ) -> Result<Option<Value>, Error> {
        let values = self.labels().values;
        let result = super::elementwise(interpreter, operator, &values, other, reflected)?;
        Ok(result.map(Array::value))
    }

    fn repr(&self, interpreter: &mut Interpreter) -> Result<String, Error> {
        super::display::index_text(interpreter, &self.labels())
    }
}

/// `MultiIndex.set_levels(levels)`: replaces the distinct values of each level by the new
/// values at the same place in the sorted order.
fn set_levels(
    interpreter: &mut Interpreter,
    labels: Labels,
    mut args: Args,
) -> Result<Value, Error> {
    let new = args
        .take(0, "levels")
        .ok_or_else(|| Error::type_error("set_levels() missing levels"))?;
    let only = args.take(1, "level");
    let old = labels.sorted_levels(interpreter)?;
    let mut new_levels: Vec<Option<Vec<Value>>> = vec![None; labels.levels()];
    match only {
        Some(level) if !level.is_none() => {
            let level = labels
                .level_of(&level)
                .ok_or_else(|| Error::key_error(level.clone()))?;
            new_levels[level] = Some(seq_values(interpreter, &new)?);
        }
        _ => {
            for (level, values) in interpreter.iterate(&new)?.into_iter().enumerate() {
                if level < new_levels.len() {
                    new_levels[level] = Some(seq_values(interpreter, &values)?);
                }
            }
        }
    }
    let mut levels = Vec::new();
    for (level, replacement) in new_levels.into_iter().enumerate() {
        let values = labels.level(level);
        let Some(replacement) = replacement else {
            levels.push(values);
            continue;
        };
        let mapped = values
            .into_iter()
            .map(|value| {
                old[level]
                    .iter()
                    .position(|known| key_equal(known, &value))
                    .and_then(|position| replacement.get(position).cloned())
                    .unwrap_or(value)
            })
            .collect();
        levels.push(mapped);
    }
    Ok(IndexObject::value(Labels::from_levels(
        levels,
        labels.names,
    )))
}
