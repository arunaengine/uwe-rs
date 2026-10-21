//! `==`, `<` and `<=>` as Nextflow's `LangHelpers` and Groovy's `DefaultTypeTransformation` do them.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cmp::Ordering;
use std::rc::Rc;

use super::{ListKind, State, Time, Value};
use crate::exception::Exception;
use crate::number::Number;
use crate::units::{parse_duration, parse_memory};

/// The relational operators, which Nextflow rewrites to `LangHelpers.compareLessThan` and friends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Relation {
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
}

impl Relation {
    fn holds(self, order: Ordering) -> bool {
        match self {
            Relation::Less => order.is_lt(),
            Relation::LessEqual => order.is_le(),
            Relation::Greater => order.is_gt(),
            Relation::GreaterEqual => order.is_ge(),
        }
    }
}

impl Value {
    /// `==` in a Nextflow script (`LangHelpers.compareEqual`). It fails only when a zero
    /// `MemoryUnit` or `Duration` meets Nextflow's `assert`.
    pub fn compare_equal(&self, other: &Value) -> Result<bool, Exception> {
        match (self, other) {
            (Value::Number(Number::Integer(a)), Value::Number(Number::Integer(b))) => Ok(a == b),
            (Value::Number(Number::Long(a)), Value::Number(Number::Long(b))) => Ok(a == b),
            // Java's `Double.equals`: `NaN` equals itself, `0.0` does not equal `-0.0`.
            (Value::Number(Number::Double(_)), Value::Number(Number::Double(_))) => {
                Ok(self.java_equals(other))
            }
            (Value::Null, Value::MemoryUnit(_) | Value::Duration(_))
            | (Value::MemoryUnit(_) | Value::Duration(_), Value::Null) => Ok(false),
            (Value::MemoryUnit(_) | Value::Duration(_), _) => {
                Ok(unit_compare(self, other)?.is_eq())
            }
            (_, Value::MemoryUnit(_) | Value::Duration(_)) => {
                Ok(unit_compare(other, self)?.is_eq())
            }
            _ => Ok(self.groovy_equals(other)),
        }
    }

    /// `<`, `<=`, `>` and `>=` in a Nextflow script (`LangHelpers.compareLessThan` and friends).
    /// A `null` next to a `MemoryUnit` or `Duration` makes every relation false.
    pub fn compare_relation(&self, relation: Relation, other: &Value) -> Result<bool, Exception> {
        match (self, other) {
            (Value::Number(Number::Double(a)), Value::Number(Number::Double(b))) => {
                Ok(a.partial_cmp(b).is_some_and(|order| relation.holds(order)))
            }
            (Value::Null, Value::MemoryUnit(_) | Value::Duration(_))
            | (Value::MemoryUnit(_) | Value::Duration(_), Value::Null) => Ok(false),
            (Value::MemoryUnit(_) | Value::Duration(_), _) => {
                Ok(relation.holds(unit_compare(self, other)?))
            }
            (_, Value::MemoryUnit(_) | Value::Duration(_)) => {
                Ok(relation.holds(unit_compare(other, self)?.reverse()))
            }
            _ => Ok(relation.holds(self.groovy_compare(other)?)),
        }
    }

    /// `<=>`: Groovy's `compareTo`, without Nextflow's rules for units.
    pub fn spaceship(&self, other: &Value) -> Result<Value, Exception> {
        let order = self.groovy_compare(other)?;
        Ok(Value::integer(order as i32))
    }

    /// Groovy's `DefaultTypeTransformation.compareEqual`, which `==` falls back on and which
    /// compares the elements of lists, sets and maps.
    pub fn groovy_equals(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Null, Value::Null) => true,
            (Value::Null, _) | (_, Value::Null) => false,
            _ if comparable(self) => compare_values(self, other, true).is_ok_and(Ordering::is_eq),
            (Value::List(left), Value::List(right)) => {
                if Rc::ptr_eq(left, right) {
                    return true;
                }
                let (left, right) = (left.borrow(), right.borrow());
                if left.kind == ListKind::Array && right.kind == ListKind::Array {
                    return left.items.len() == right.items.len()
                        && left
                            .items
                            .iter()
                            .zip(&right.items)
                            .all(|(a, b)| a.groovy_equals(b));
                }
                left.items.len() == right.items.len()
                    && left
                        .items
                        .iter()
                        .zip(&right.items)
                        .all(|(a, b)| coerced_equals(a, b))
            }
            (Value::MapEntry(left), Value::MapEntry(right)) => {
                left.0.java_equals(&right.0) && left.1.groovy_equals(&right.1)
            }
            (Value::Set(left), Value::Set(right)) => {
                let (left, right) = (left.borrow(), right.borrow());
                if left.items.len() != right.items.len() {
                    return false;
                }
                let mut unmatched: Vec<&Value> = right.items.iter().map(|item| &item.0).collect();
                left.items.iter().all(|item| {
                    let found = unmatched
                        .iter()
                        .position(|other| coerced_equals(&item.0, other));
                    found.map(|index| unmatched.remove(index)).is_some()
                })
            }
            (Value::Map(left), Value::Map(right)) => {
                let (left, right) = (left.borrow(), right.borrow());
                left.entries.len() == right.entries.len()
                    && left
                        .entries
                        .keys()
                        .all(|key| right.entries.contains_key(key))
                    && left.entries.iter().all(|(key, value)| {
                        coerced_equals(value, right.entries.get(key).unwrap_or(&Value::Null))
                    })
            }
            _ => self.java_equals(other),
        }
    }

    /// Groovy's `DefaultTypeTransformation.compareTo`: `null` first, numbers by value, strings
    /// by UTF-16 code units, and "Cannot compare" for classes that do not compare.
    pub fn groovy_compare(&self, other: &Value) -> Result<Ordering, Exception> {
        compare_values(self, other, false)
    }

    /// Groovy's `coercedEquals`, which `unique()`, `intersect` and collection equality use.
    pub fn coerced_equals(&self, other: &Value) -> bool {
        coerced_equals(self, other)
    }

    /// Groovy's `NumberAwareComparator`, the default order of `sort()`.
    pub fn number_aware_compare(&self, other: &Value) -> Ordering {
        number_aware_compare(self, other)
    }
}

/// Whether the Java class implements `Comparable`.
fn comparable(value: &Value) -> bool {
    matches!(
        value,
        Value::Boolean(_)
            | Value::Number(_)
            | Value::Character(_)
            | Value::String(_)
            | Value::GString(_)
            | Value::MemoryUnit(_)
            | Value::Duration(_)
            | Value::Path(_)
    ) || time(value).is_some()
}

/// A `java.time` value, which compares with values of its own class.
fn time(value: &Value) -> Option<Time> {
    match value {
        Value::Object(object) => match object.borrow().state {
            State::Time(time) => Some(time),
            _ => None,
        },
        _ => None,
    }
}

/// `compareToWithEqualityCheck`. For an equality check, classes that do not compare give
/// "not equal" instead of an error.
fn compare_values(left: &Value, right: &Value, equality: bool) -> Result<Ordering, Exception> {
    let cannot = || {
        Exception::illegal_argument(format!(
            "Cannot compare {} with value '{}' and {} with value '{}'",
            left.java_class_name(),
            left.to_java_string(),
            right.java_class_name(),
            right.to_java_string()
        ))
    };
    if let (Some(a), Some(b)) = (time(left), time(right))
        && std::mem::discriminant(&a) == std::mem::discriminant(&b)
    {
        return Ok(a.cmp(&b));
    }
    match (left, right) {
        (Value::Null, Value::Null) => Ok(Ordering::Equal),
        (Value::Null, _) => Ok(Ordering::Less),
        (_, Value::Null) => Ok(Ordering::Greater),
        (Value::Number(a), Value::Number(b)) => Ok(a.compare(b)),
        (Value::Number(a), text) if character(text).is_some() => {
            Ok(a.compare(&character(text).unwrap_or(Number::Integer(0))))
        }
        (text, Value::Number(b)) if character(text).is_some() => {
            Ok(character(text).unwrap_or(Number::Integer(0)).compare(b))
        }
        (Value::String(a) | Value::GString(a), Value::String(b) | Value::GString(b)) => {
            Ok(a.encode_utf16().cmp(b.encode_utf16()))
        }
        (Value::Character(_), _) | (_, Value::Character(_))
            if character(left).is_some() && character(right).is_some() =>
        {
            let code = |value: &Value| character(value).map_or(0, |code| code.to_i64());
            Ok(code(left).cmp(&code(right)))
        }
        (Value::Boolean(a), Value::Boolean(b)) => Ok(a.cmp(b)),
        (Value::Path(a), Value::Path(b)) => Ok(a.uri().cmp(&b.uri())),
        (Value::MemoryUnit(a), Value::MemoryUnit(b)) | (Value::Duration(a), Value::Duration(b)) => {
            Ok(a.cmp(b))
        }
        _ if equality => Ok(Ordering::Less),
        _ => Err(cannot()),
    }
}

/// The character code of a character or a one-character string, which Groovy compares with
/// numbers.
fn character(value: &Value) -> Option<Number> {
    match value {
        Value::Character(unit) => Some(Number::Integer(i32::from(*unit))),
        Value::String(text) | Value::GString(text) => {
            let mut units = text.encode_utf16();
            match (units.next(), units.next()) {
                (Some(unit), None) => Some(Number::Integer(i32::from(unit))),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Groovy's `coercedEquals` for elements: comparable values must also compare as equal.
fn coerced_equals(left: &Value, right: &Value) -> bool {
    if matches!(left, Value::Null) {
        return matches!(right, Value::Null);
    }
    if comparable(left)
        && !(comparable(right) && number_aware_compare(left, right) == Ordering::Equal)
    {
        return false;
    }
    left.groovy_equals(right)
}

/// Groovy's `NumberAwareComparator`: `compareTo`, else an order by hash codes that is never equal
/// unless the values are Java-equal.
fn number_aware_compare(left: &Value, right: &Value) -> Ordering {
    if let Ok(order) = left.groovy_compare(right) {
        return order;
    }
    let (a, b) = (left.java_hash(), right.java_hash());
    if a == b && left.java_equals(right) {
        Ordering::Equal
    } else if a > b {
        Ordering::Greater
    } else {
        Ordering::Less
    }
}

/// `MemoryUnit.compareTo(unit, other)` or `Duration.compareTo(unit, other)`: the other side may
/// be a unit, a number or text. Nextflow first asserts that the unit is not zero.
fn unit_compare(unit: &Value, other: &Value) -> Result<Ordering, Exception> {
    let (count, memory) = match unit {
        Value::MemoryUnit(size) => (*size, true),
        Value::Duration(millis) => (*millis, false),
        _ => unreachable!("only called with units"),
    };
    if count == 0 {
        let text = unit.to_java_string();
        return Err(Exception::assertion(format!(
            "assert left\n       |\n       {text}"
        )));
    }
    let other_count = match other {
        Value::MemoryUnit(size) if memory => *size,
        Value::Duration(millis) if !memory => *millis,
        Value::Number(number) => number.to_i64(),
        Value::String(text) | Value::GString(text) if memory => parse_memory(text)?,
        Value::String(text) | Value::GString(text) => parse_duration(text)?,
        _ => {
            let name = if memory { "memory" } else { "duration" };
            return Err(Exception::illegal_argument(format!(
                "Not a valid {name} value: {}",
                other.to_interpolated()
            )));
        }
    };
    Ok(count.cmp(&other_count))
}
