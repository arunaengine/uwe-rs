//! Ranges from `a..b` and `a..<b`, as Groovy's `ScriptBytecodeAdapter.createRange` builds them.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::rc::Rc;

use super::{List, ListKind, Operator, Value};
use crate::exception::Exception;
use crate::number::Number;

/// The most elements a range may have; iunctus builds the elements when the range is made.
const LARGEST: usize = 10_000_000;

/// The bounds of a range as written. Its elements live in the list that holds it.
#[derive(Debug)]
pub struct Range {
    pub from: Value,
    pub to: Value,
    /// `a..<b`, which leaves out `b`.
    pub exclusive: bool,
    pub class: RangeClass,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RangeClass {
    /// `groovy.lang.IntRange`: two `Integer` ends.
    Int,
    /// `groovy.lang.NumberRange`: two numbers, stepping by 1.
    Number,
    /// `groovy.lang.ObjectRange`, like `'a'..'c'`, stepping with `next()`.
    Object,
    /// `groovy.lang.EmptyRange`: `a..<a`.
    Empty,
}

impl RangeClass {
    pub fn name(self) -> &'static str {
        match self {
            RangeClass::Int => "IntRange",
            RangeClass::Number => "NumberRange",
            RangeClass::Object => "ObjectRange",
            RangeClass::Empty => "EmptyRange",
        }
    }
}

impl Value {
    /// `from..to` or `from..<to`. A range counts down when `from` is larger.
    pub fn range(from: Value, to: Value, exclusive: bool) -> Result<Value, Exception> {
        // Groovy's messages for a range with a null end.
        for (end, name) in [(&from, "from"), (&to, "to")] {
            if matches!(end, Value::Null) {
                let message =
                    format!("Must specify a non-null value for the '{name}' index in a Range");
                return Err(Exception::illegal_argument(message));
            }
        }
        let class = match (&from, &to) {
            _ if exclusive && from.compare_equal(&to)? => RangeClass::Empty,
            (Value::Number(Number::Integer(_)), Value::Number(Number::Integer(_))) => {
                RangeClass::Int
            }
            (Value::Number(_), Value::Number(_)) => RangeClass::Number,
            (Value::String(_) | Value::GString(_), Value::String(_) | Value::GString(_)) => {
                RangeClass::Object
            }
            _ => {
                return Err(Exception::illegal_argument(format!(
                    "Unsupported range from {} to {}",
                    from.class_name(),
                    to.class_name()
                )));
            }
        };
        let items = match class {
            RangeClass::Empty => Vec::new(),
            _ => steps(&from, &to, exclusive)?,
        };
        let range = Rc::new(Range {
            from,
            to,
            exclusive,
            class,
        });
        let list = List {
            kind: ListKind::Range,
            items,
            range: Some(range),
        };
        Ok(Value::List(Rc::new(RefCell::new(list))))
    }
}

impl Range {
    /// `toString()`: `from..to` or `from..<to`.
    pub fn text(&self) -> String {
        let separator = if self.exclusive { "..<" } else { ".." };
        format!(
            "{}{separator}{}",
            self.from.to_java_string(),
            self.to.to_java_string()
        )
    }

    /// `range.isCase(item)`: an `IntRange` takes `Integer` and `BigInteger` values between its
    /// ends; other ranges compare each element with `==`.
    pub fn contains(&self, items: &[Value], item: &Value) -> bool {
        match (self.class, item) {
            (
                RangeClass::Int,
                Value::Number(number @ (Number::Integer(_) | Number::BigInteger(_))),
            ) => {
                let (Some(first), Some(last)) = (items.first(), items.last()) else {
                    return false;
                };
                let (low, high) = match (first, last) {
                    (Value::Number(a), Value::Number(b)) if a.compare(b).is_le() => (a, b),
                    (Value::Number(a), Value::Number(b)) => (b, a),
                    _ => return false,
                };
                number.compare(low).is_ge() && number.compare(high).is_le()
            }
            (RangeClass::Int, _) => false,
            _ => items.iter().any(|element| element.groovy_equals(item)),
        }
    }
}

/// The elements from `from` toward `to`, adding or subtracting 1 or calling `next()`.
fn steps(from: &Value, to: &Value, exclusive: bool) -> Result<Vec<Value>, Exception> {
    let descending = from.groovy_compare(to)?.is_gt();
    let mut items = Vec::new();
    let mut current = from.clone();
    loop {
        let order = current.groovy_compare(to)?;
        let past = if descending {
            order.is_lt()
        } else {
            order.is_gt()
        };
        if past || (exclusive && order.is_eq()) {
            return Ok(items);
        }
        if items.len() == LARGEST {
            return Err(Exception::illegal_argument(format!(
                "Ranges of more than {LARGEST} elements are not supported"
            )));
        }
        items.push(current.clone());
        if order.is_eq() {
            return Ok(items);
        }
        current = match &current {
            Value::Number(_) => {
                let operator = if descending {
                    Operator::Subtract
                } else {
                    Operator::Add
                };
                current.apply(operator, &Value::integer(1))?
            }
            Value::String(text) | Value::GString(text) => Value::string(next(text, descending)),
            _ => return Ok(items),
        };
    }
}

/// Groovy's `String.next()` and `previous()`: the last character moves by one.
fn next(text: &str, backwards: bool) -> String {
    let mut characters: Vec<char> = text.chars().collect();
    match characters.last_mut() {
        Some(last) => {
            let code = *last as u32;
            let moved = if backwards {
                code.saturating_sub(1)
            } else {
                code + 1
            };
            *last = char::from_u32(moved).unwrap_or(*last);
        }
        None => characters.push(if backwards { '\u{0}' } else { '\u{1}' }),
    }
    characters.into_iter().collect()
}
