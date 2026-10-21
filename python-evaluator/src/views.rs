//! The views of a dict that `keys()`, `values()` and `items()` give: they read the dict as it
//! is when they are used, without a copy.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;

use indexmap::IndexSet;

use crate::error::Error;
use crate::interpreter::{Interpreter, Operator};
use crate::value::{Dict, Native, Value};

/// What a view of a dict shows.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Part {
    Keys,
    Values,
    Items,
}

/// `dict_keys`, `dict_values` or `dict_items` of a dict.
pub struct View {
    pub dict: Rc<RefCell<Dict>>,
    pub part: Part,
}

impl View {
    /// The members of the view as the dict holds them now.
    fn members(&self) -> Vec<Value> {
        let dict = self.dict.borrow();
        let entries = dict.entries.iter();
        match self.part {
            Part::Keys => entries.map(|(key, _)| key.0.clone()).collect(),
            Part::Values => entries.map(|(_, value)| value.clone()).collect(),
            Part::Items => entries
                .map(|(key, value)| Value::tuple(vec![key.0.clone(), value.clone()]))
                .collect(),
        }
    }

    /// The members as a set, for the set operators that keys and items views take.
    fn as_set(&self, interpreter: &mut Interpreter) -> Result<Value, Error> {
        set_of(interpreter, self.members())
    }
}

fn set_of(interpreter: &mut Interpreter, items: Vec<Value>) -> Result<Value, Error> {
    let mut members = IndexSet::new();
    for item in items {
        let key = interpreter.key(&item)?;
        interpreter.add_member(&mut members, key)?;
    }
    Ok(Value::set(members))
}

impl Native for View {
    fn type_name(&self) -> &str {
        match self.part {
            Part::Keys => "dict_keys",
            Part::Values => "dict_values",
            Part::Items => "dict_items",
        }
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
        let this = this.clone();
        Ok(match name {
            "__len__" => {
                let dict = self.dict.clone();
                Some(Value::builtin("__len__", move |_, _| {
                    Ok(Value::Int(dict.borrow().entries.len() as i64))
                }))
            }
            "__iter__" => {
                let view = View {
                    dict: self.dict.clone(),
                    part: self.part,
                };
                Some(Value::builtin("__iter__", move |_, _| {
                    Ok(Value::iterator(view.members()))
                }))
            }
            "__contains__" => Some(Value::builtin("__contains__", move |interpreter, args| {
                args.expect("__contains__", 1, 1)?;
                interpreter
                    .contains(&this, &args.positional[0])
                    .map(Value::Bool)
            })),
            "isdisjoint" => Some(Value::builtin("isdisjoint", move |interpreter, args| {
                args.expect("isdisjoint", 1, 1)?;
                for item in interpreter.iterate(&args.positional[0])? {
                    if interpreter.contains(&this, &item)? {
                        return Ok(Value::Bool(false));
                    }
                }
                Ok(Value::Bool(true))
            })),
            _ => None,
        })
    }

    fn iterate(&self, _: &mut Interpreter) -> Result<Option<Vec<Value>>, Error> {
        Ok(Some(self.members()))
    }

    fn length(&self) -> Option<usize> {
        Some(self.dict.borrow().entries.len())
    }

    fn truthy(&self) -> Option<bool> {
        Some(!self.dict.borrow().entries.is_empty())
    }

    fn contains(&self, interpreter: &mut Interpreter, item: &Value) -> Result<Option<bool>, Error> {
        match self.part {
            Part::Keys => {
                let key = interpreter.dict_key(&self.dict, item)?;
                Ok(Some(self.dict.borrow().entries.contains_key(&key)))
            }
            Part::Items => {
                let Value::Tuple(pair) = item.plain() else {
                    return Ok(Some(false));
                };
                let [key, value] = &pair[..] else {
                    return Ok(Some(false));
                };
                let key = interpreter.dict_key(&self.dict, key)?;
                let found = self.dict.borrow().entries.get(&key).cloned();
                match found {
                    Some(found) => interpreter.equals(&found, value).map(Some),
                    None => Ok(Some(false)),
                }
            }
            Part::Values => {
                for member in self.members() {
                    if interpreter.equals(&member, item)? {
                        return Ok(Some(true));
                    }
                }
                Ok(Some(false))
            }
        }
    }

    fn repr(&self, interpreter: &mut Interpreter) -> Result<String, Error> {
        let parts = (self.members().iter())
            .map(|member| interpreter.repr(member))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(format!("{}([{}])", self.type_name(), parts.join(", ")))
    }

    /// The set operators of keys and items views, which give sets, and their comparison
    /// with sets; a values view equals only itself.
    fn binary(
        &self,
        interpreter: &mut Interpreter,
        operator: Operator,
        other: &Value,
        reflected: bool,
    ) -> Result<Option<Value>, Error> {
        if self.part == Part::Values {
            return Ok(None);
        }
        let set_like = |value: &Value| match value.plain() {
            Value::Set(_) | Value::FrozenSet(_) => true,
            Value::Native(object) => (object.as_any().downcast_ref::<View>())
                .is_some_and(|view| view.part != Part::Values),
            _ => false,
        };
        let other_set = |interpreter: &mut Interpreter| match other.plain() {
            Value::Native(object) if object.as_any().is::<View>() => {
                let view = object.as_any().downcast_ref::<View>().expect("a view");
                view.as_set(interpreter)
            }
            Value::Set(_) | Value::FrozenSet(_) => Ok(other.plain().clone()),
            _ => {
                let items = interpreter.iterate(other)?;
                set_of(interpreter, items)
            }
        };
        match operator {
            Operator::BitOr | Operator::BitAnd | Operator::Sub | Operator::BitXor => {
                let this = self.as_set(interpreter)?;
                let that = other_set(interpreter)?;
                let (left, right) = if reflected {
                    (that, this)
                } else {
                    (this, that)
                };
                interpreter.binary(operator, &left, &right).map(Some)
            }
            Operator::Eq | Operator::NotEq => {
                let same = set_like(other) && {
                    let this = self.as_set(interpreter)?;
                    let that = other_set(interpreter)?;
                    interpreter.equals(&this, &that)?
                };
                Ok(Some(Value::Bool(same == (operator == Operator::Eq))))
            }
            _ => Ok(None),
        }
    }

    fn hashable(&self) -> bool {
        false
    }
}
