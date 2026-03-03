//! Groovy's arithmetic, shift and bit operators on all values, and the `in` operator.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::rc::Rc;

use super::{Key, List, ListKind, Map, MapKind, Set, Value};
use crate::exception::Exception;
use crate::number::Number;
use crate::units::{check_count, divide_count, multiply_count};

/// The binary operators that Groovy turns into method calls like `plus` and `leftShift`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operator {
    Add,
    Subtract,
    Multiply,
    Divide,
    Modulo,
    Power,
    LeftShift,
    RightShift,
    UnsignedShift,
    BitAnd,
    BitOr,
    Xor,
}

impl Operator {
    /// The Groovy method that implements the operator.
    pub fn method(self) -> &'static str {
        match self {
            Operator::Add => "plus",
            Operator::Subtract => "minus",
            Operator::Multiply => "multiply",
            Operator::Divide => "div",
            Operator::Modulo => "mod",
            Operator::Power => "power",
            Operator::LeftShift => "leftShift",
            Operator::RightShift => "rightShift",
            Operator::UnsignedShift => "rightShiftUnsigned",
            Operator::BitAnd => "and",
            Operator::BitOr => "or",
            Operator::Xor => "xor",
        }
    }
}

impl Value {
    /// Applies a binary operator as Groovy dispatches it on the classes of both sides.
    pub fn apply(&self, operator: Operator, other: &Value) -> Result<Value, Exception> {
        use Operator::*;
        let result = match (operator, self, other) {
            (_, Value::Number(left), Value::Number(right)) => {
                return number_operator(operator, left, right).map(Value::Number);
            }
            (Add, Value::String(left), right) => Value::string(format!("{left}{right}")),
            (Add, Value::GString(left), right) => Value::GString(format!("{left}{right}").into()),
            (Add, Value::Number(left), Value::String(right) | Value::GString(right)) => {
                Value::string(format!("{left}{right}"))
            }
            (Add, Value::Null, Value::String(right) | Value::GString(right)) => {
                Value::string(format!("null{right}"))
            }
            (Subtract, Value::String(left) | Value::GString(left), right) => {
                Value::string(left.replacen(&right.to_groovy_string(), "", 1))
            }
            (Multiply, Value::String(left) | Value::GString(left), Value::Number(times)) => {
                let times = times.to_i32();
                if times < 0 {
                    return Err(Exception::illegal_argument(format!(
                        "multiply() should be called with a number of 0 or greater not: {times}"
                    )));
                }
                Value::string(left.repeat(times as usize))
            }
            (Add, Value::List(left), right) => {
                let mut items = left.borrow().items.clone();
                items.extend(elements_or_self(right));
                similar_list(left, items)?
            }
            (Subtract, Value::List(left), right) => {
                let items = left.borrow().items.clone();
                similar_list(left, remove_all(items, right))?
            }
            (Multiply, Value::List(left), Value::Number(times)) => {
                let items = left.borrow().items.clone();
                let times = times.to_i32().max(0) as usize;
                similar_list(left, (0..times).flat_map(|_| items.clone()).collect())?
            }
            (LeftShift, Value::List(list), item) => {
                let mut list_ref = list.borrow_mut();
                if matches!(
                    list_ref.kind,
                    ListKind::Tuple | ListKind::Array | ListKind::Range
                ) {
                    return Err(unsupported_change());
                }
                list_ref.items.push(item.clone());
                return Ok(self.clone());
            }
            (Add | Subtract, Value::Set(left), right) => {
                let mut items = left.borrow().items.clone();
                let others = elements_or_self(right);
                if operator == Add {
                    items.extend(others.into_iter().map(Key));
                } else {
                    items.retain(|item| !others.iter().any(|other| item.0.groovy_equals(other)));
                }
                Value::Set(Rc::new(RefCell::new(Set {
                    kind: left.borrow().kind,
                    items,
                })))
            }
            (LeftShift, Value::Set(set), item) => {
                set.borrow_mut().items.insert(Key(item.clone()));
                return Ok(self.clone());
            }
            (Add, Value::Map(left), Value::Map(right)) => {
                let (left_map, right_map) = (left.borrow(), right.borrow());
                if left_map.kind == MapKind::Record && right_map.kind != MapKind::Record {
                    return Err(unsupported_change());
                }
                let mut entries = left_map.entries.clone();
                for (key, value) in &right_map.entries {
                    entries.insert(key.clone(), value.clone());
                }
                new_map(left_map.kind, entries)
            }
            (Subtract, Value::Map(left), Value::Map(right)) => {
                let (left_map, right_map) = (left.borrow(), right.borrow());
                let mut entries = left_map.entries.clone();
                entries.retain(|key, value| {
                    right_map
                        .entries
                        .get(key)
                        .is_none_or(|other| !value.java_equals(other))
                });
                new_map(left_map.kind, entries)
            }
            (LeftShift, Value::Map(map), Value::Map(other)) => {
                let entries = other.borrow().entries.clone();
                let mut map_ref = map.borrow_mut();
                if map_ref.kind == MapKind::Record {
                    return Err(unsupported_change());
                }
                map_ref.entries.extend(entries);
                return Ok(self.clone());
            }
            (Add | Subtract, Value::MemoryUnit(left), Value::MemoryUnit(right)) => {
                let count = if operator == Add {
                    left + right
                } else {
                    left - right
                };
                Value::MemoryUnit(check_count(count, "Memory")?)
            }
            (Add | Subtract, Value::MemoryUnit(_), Value::Null) => self.clone(),
            (Add | Subtract, Value::Duration(left), Value::Duration(right)) => {
                let count = if operator == Add {
                    left + right
                } else {
                    left - right
                };
                Value::Duration(check_count(count, "Duration")?)
            }
            (Multiply, Value::MemoryUnit(size), Value::Number(factor))
            | (Multiply, Value::Number(factor), Value::MemoryUnit(size)) => {
                Value::MemoryUnit(multiply_count(*size, factor, "Memory")?)
            }
            (Multiply, Value::Duration(millis), Value::Number(factor))
            | (Multiply, Value::Number(factor), Value::Duration(millis)) => {
                Value::Duration(multiply_count(*millis, factor, "Duration")?)
            }
            (Divide, Value::MemoryUnit(size), Value::Number(divisor)) => {
                Value::MemoryUnit(divide_count(*size, divisor, "Memory")?)
            }
            (Divide, Value::Duration(millis), Value::Number(divisor)) => {
                Value::Duration(divide_count(*millis, divisor, "Duration")?)
            }
            (BitAnd, Value::Boolean(left), Value::Boolean(right)) => Value::Boolean(*left & *right),
            (BitOr, Value::Boolean(left), Value::Boolean(right)) => Value::Boolean(*left | *right),
            (Xor, Value::Boolean(left), Value::Boolean(right)) => Value::Boolean(*left ^ *right),
            (Add, Value::Null, right) => {
                let message = format!("Cannot execute null+{}", right.to_java_string());
                return Err(Exception::new("NullPointerException", message));
            }
            (_, Value::Null, _) => {
                let method = operator.method();
                let message = format!("Cannot invoke method {method}() on null object");
                return Err(Exception::new("NullPointerException", message));
            }
            _ => return Err(no_signature(self, operator.method(), other)),
        };
        Ok(result)
    }

    /// Unary `-`: numbers keep their class, and an `ArrayList` negates each element.
    pub fn negate(&self) -> Result<Value, Exception> {
        match self {
            Value::Number(number) => Ok(Value::Number(number.negate())),
            Value::List(list) if list.borrow().kind == ListKind::ArrayList => {
                let items: Result<Vec<Value>, Exception> =
                    list.borrow().items.iter().map(Value::negate).collect();
                Ok(Value::list(items?))
            }
            _ => Err(no_signature(self, "negative", &Value::Null)),
        }
    }

    /// `a in b`, which is `b.isCase(a)`: membership by Java equality for collections, a
    /// truthy value for maps, equal text for strings and equal value for numbers.
    pub fn is_case(&self, item: &Value) -> Result<bool, Exception> {
        Ok(match self {
            Value::Null => matches!(item, Value::Null),
            Value::List(list) => {
                let list = list.borrow();
                match &list.range {
                    Some(range) => range.contains(&list.items, item),
                    None => list.items.iter().any(|other| other.java_equals(item)),
                }
            }
            Value::Set(set) => set.borrow().items.contains(&Key(item.clone())),
            Value::Map(map) => map
                .borrow()
                .entries
                .get(&Key(item.clone()))
                .is_some_and(Value::truth),
            Value::String(text) | Value::GString(text) => {
                !matches!(item, Value::Null) && item.to_java_string() == **text
            }
            Value::Number(number) => match item {
                Value::Number(other) => number.compare(other).is_eq(),
                _ => self.java_equals(item),
            },
            _ => self.java_equals(item),
        })
    }
}

fn number_operator(operator: Operator, left: &Number, right: &Number) -> Result<Number, Exception> {
    use Operator::*;
    Ok(match operator {
        Add => left.add(right),
        Subtract => left.subtract(right),
        Multiply => left.multiply(right),
        Divide => left.divide(right)?,
        Modulo => left.modulo(right)?,
        Power => left.power(right),
        LeftShift => left.shift_left(right)?,
        RightShift => left.shift_right(right)?,
        UnsignedShift => left.shift_right_unsigned(right)?,
        BitAnd => left.and(right)?,
        BitOr => left.or(right)?,
        Xor => left.xor(right)?,
    })
}

/// The elements of a collection, or the value itself for anything else.
fn elements_or_self(value: &Value) -> Vec<Value> {
    match value {
        Value::List(list) => list.borrow().items.clone(),
        Value::Set(set) => set
            .borrow()
            .items
            .iter()
            .map(|item| item.0.clone())
            .collect(),
        other => vec![other.clone()],
    }
}

/// `list - x`: without every element that equals `x`, or any element of a collection `x`.
fn remove_all(items: Vec<Value>, removed: &Value) -> Vec<Value> {
    let removed = elements_or_self(removed);
    items
        .into_iter()
        .filter(|item| !removed.iter().any(|other| item.groovy_equals(other)))
        .collect()
}

/// A new list of the same class, as Groovy's `createSimilarList` makes it. A tuple copy
/// rejects the elements added to it.
fn similar_list(original: &Rc<RefCell<List>>, items: Vec<Value>) -> Result<Value, Exception> {
    let kind = match original.borrow().kind {
        ListKind::Tuple => return Err(unsupported_change()),
        ListKind::Array | ListKind::Range => ListKind::ArrayList,
        kind => kind,
    };
    Ok(Value::List(Rc::new(RefCell::new(List::new(kind, items)))))
}

fn new_map(kind: MapKind, entries: indexmap::IndexMap<Key, Value>) -> Value {
    Value::Map(Rc::new(RefCell::new(Map { kind, entries })))
}

fn unsupported_change() -> Exception {
    Exception::new("UnsupportedOperationException", "")
}

/// Groovy's `MissingMethodException` for an operator without a method for these classes,
/// without the list of possible solutions that Groovy appends.
pub(crate) fn no_signature(receiver: &Value, method: &str, argument: &Value) -> Exception {
    Exception::new(
        "MissingMethodException",
        format!(
            "No signature of method: {}.{method}() is applicable for argument types: ({}) values: [{}]",
            receiver.java_class_name(),
            argument.class_name(),
            argument.to_groovy_string()
        ),
    )
}
