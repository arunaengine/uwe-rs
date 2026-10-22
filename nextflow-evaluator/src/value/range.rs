//! Ranges from `a..b` and `a..<b`, as Groovy's `ScriptBytecodeAdapter.createRange` builds them.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use num_traits::ToPrimitive;

use super::{List, ListKind, Operator, Value};
use crate::exception::Exception;
use crate::number::{BigDecimal, Number};

/// The maximum materialized size of a floating-point or object range.
const LARGEST: usize = 10_000_000;

/// The bounds of a range as written; exact numeric elements are materialized only on demand.
#[derive(Debug)]
pub struct Range {
    pub from: Value,
    pub to: Value,
    /// `a..<b`, which leaves out `b`.
    pub exclusive: bool,
    pub class: RangeClass,
    pub(crate) count: Option<usize>,
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
        let count = count(&from, &to, exclusive)?;
        let items = if class == RangeClass::Empty || count.is_some() {
            Vec::new()
        } else {
            steps(&from, &to, exclusive)?
        };
        let range = Rc::new(Range {
            from,
            to,
            exclusive,
            class,
            count,
        });
        let list = List {
            kind: ListKind::Range,
            items,
            range: Some(range),
            default: None,
        };
        Ok(Value::List(Rc::new(crate::value::ListCell::new(list))))
    }
}

impl Range {
    pub(crate) fn item(&self, index: usize) -> Option<Value> {
        if index >= self.count? {
            return None;
        }
        let (Value::Number(from), Value::Number(to)) = (&self.from, &self.to) else {
            return None;
        };
        if index == 0 {
            return Some(self.from.clone());
        }
        let descending = from.compare(to).is_gt();
        let offset = if descending {
            -(index as i128)
        } else {
            index as i128
        };
        let value = match from {
            Number::Integer(value) => {
                Number::Integer(i32::try_from(i128::from(*value) + offset).ok()?)
            }
            Number::Long(value) => Number::Long(i64::try_from(i128::from(*value) + offset).ok()?),
            Number::BigInteger(value) => {
                Number::BigInteger(value + num_bigint::BigInt::from(offset))
            }
            Number::BigDecimal(value) => {
                Number::BigDecimal(value.add(&BigDecimal::from(num_bigint::BigInt::from(offset))))
            }
            _ => return None,
        };
        Some(Value::Number(value))
    }

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
                let (Some(first), Some(last)) = (
                    self.item(0),
                    self.count
                        .and_then(|count| count.checked_sub(1))
                        .and_then(|index| self.item(index)),
                ) else {
                    return false;
                };
                let (low, high) = match (&first, &last) {
                    (Value::Number(a), Value::Number(b)) if a.compare(b).is_le() => (a, b),
                    (Value::Number(a), Value::Number(b)) => (b, a),
                    _ => return false,
                };
                number.compare(low).is_ge() && number.compare(high).is_le()
            }
            (RangeClass::Int, _) => false,
            (RangeClass::Number, Value::Number(number))
                if self.count.is_some()
                    && !matches!(number, Number::Float(_) | Number::Double(_)) =>
            {
                let Value::Number(from) = &self.from else {
                    return false;
                };
                let offset = number.to_big_decimal().subtract(&from.to_big_decimal());
                let offset = if from
                    .compare(match &self.to {
                        Value::Number(to) => to,
                        _ => from,
                    })
                    .is_gt()
                {
                    offset.negate()
                } else {
                    offset
                };
                let integer = offset.to_bigint();
                offset.compare(&BigDecimal::from(integer.clone())).is_eq()
                    && integer
                        .to_usize()
                        .and_then(|index| self.item(index))
                        .is_some_and(|value| value.groovy_equals(item))
            }
            (_, _) if self.count.is_some() => (0..self.count.unwrap_or(0))
                .filter_map(|index| self.item(index))
                .any(|value| value.groovy_equals(item)),
            _ => items.iter().any(|element| element.groovy_equals(item)),
        }
    }
}

/// Exact numeric ranges retain bounds; floating-point and object stepping stays unchanged.
fn count(from: &Value, to: &Value, exclusive: bool) -> Result<Option<usize>, Exception> {
    let (Value::Number(from), Value::Number(to)) = (from, to) else {
        return Ok(None);
    };
    if matches!(from, Number::Float(_) | Number::Double(_))
        || matches!(to, Number::Float(_) | Number::Double(_))
    {
        return Ok(None);
    }
    let end = to.to_big_decimal();
    let bounds = match from {
        Number::Integer(_) => Some((i64::from(i32::MIN), i64::from(i32::MAX))),
        Number::Long(_) => Some((i64::MIN, i64::MAX)),
        _ => None,
    };
    if let Some((low, high)) = bounds
        && (end
            .compare(&BigDecimal::from(num_bigint::BigInt::from(low)))
            .is_lt()
            || end
                .compare(&BigDecimal::from(num_bigint::BigInt::from(high)))
                .is_gt())
    {
        return Ok(None);
    }
    let difference = end.subtract(&from.to_big_decimal());
    let difference = if difference.signum() < 0 {
        difference.negate()
    } else {
        difference
    };
    let whole = difference.to_bigint();
    let exact = difference.compare(&BigDecimal::from(whole.clone())).is_eq();
    let length = whole + num_bigint::BigInt::from(usize::from(!exclusive || !exact));
    length.to_usize().map(Some).ok_or_else(|| {
        Exception::illegal_argument("Range length exceeds the supported address space")
    })
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
