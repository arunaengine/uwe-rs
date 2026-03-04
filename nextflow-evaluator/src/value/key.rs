//! Java's `equals` and `hashCode`, which decide map keys, set members and `contains`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::hash::{Hash, Hasher};
use std::rc::Rc;

use num_bigint::{BigInt, Sign};

use super::{ListKind, Value};
use crate::number::Number;

/// A value used as a map key or set member, compared with Java's `equals` and `hashCode`.
/// So `Integer` 1 and `Long` 1 are different keys, and so are `1.0` and `1.00`.
#[derive(Clone, Debug)]
pub struct Key(pub Value);

impl PartialEq for Key {
    fn eq(&self, other: &Self) -> bool {
        self.0.java_equals(&other.0)
    }
}

impl Eq for Key {}

impl Hash for Key {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_i32(self.0.java_hash());
    }
}

impl Value {
    /// Java's `equals`: the same class and the same content. Lists of any kind but arrays are
    /// equal when their elements are; maps and sets ignore order.
    pub fn java_equals(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Null, Value::Null) => true,
            (Value::Boolean(left), Value::Boolean(right)) => left == right,
            (Value::Number(left), Value::Number(right)) => number_equals(left, right),
            (Value::String(left), Value::String(right)) => left == right,
            (Value::GString(left), Value::GString(right)) => left == right,
            (Value::List(left), Value::List(right)) => {
                if Rc::ptr_eq(left, right) {
                    return true;
                }
                let (left, right) = (left.borrow(), right.borrow());
                left.kind != ListKind::Array
                    && right.kind != ListKind::Array
                    && left.items.len() == right.items.len()
                    && left
                        .items
                        .iter()
                        .zip(&right.items)
                        .all(|(a, b)| a.java_equals(b))
            }
            (Value::Map(left), Value::Map(right)) => {
                if Rc::ptr_eq(left, right) {
                    return true;
                }
                let (left, right) = (left.borrow(), right.borrow());
                left.entries.len() == right.entries.len()
                    && left.entries.iter().all(|(key, value)| {
                        right
                            .entries
                            .get(key)
                            .is_some_and(|other| value.java_equals(other))
                    })
            }
            (Value::Set(left), Value::Set(right)) => {
                if Rc::ptr_eq(left, right) {
                    return true;
                }
                let (left, right) = (left.borrow(), right.borrow());
                left.items.len() == right.items.len()
                    && left.items.iter().all(|item| right.items.contains(item))
            }
            (Value::MapEntry(left), Value::MapEntry(right)) => {
                left.0.java_equals(&right.0) && left.1.java_equals(&right.1)
            }
            (Value::MemoryUnit(left), Value::MemoryUnit(right)) => left == right,
            (Value::Duration(left), Value::Duration(right)) => left == right,
            (Value::Closure(left), Value::Closure(right)) => Rc::ptr_eq(left, right),
            (Value::Exception(left), Value::Exception(right)) => Rc::ptr_eq(left, right),
            (Value::Pattern(left), Value::Pattern(right)) => Rc::ptr_eq(left, right),
            (Value::Matcher(left), Value::Matcher(right)) => Rc::ptr_eq(left, right),
            _ => false,
        }
    }

    /// Java's `hashCode`. It must agree with `java_equals`, and it decides the iteration order
    /// of a `HashSet`.
    pub fn java_hash(&self) -> i32 {
        match self {
            Value::Null => 0,
            Value::Boolean(value) => {
                if *value {
                    1231
                } else {
                    1237
                }
            }
            Value::Number(number) => number_hash(number),
            Value::String(text) => string_hash(text),
            Value::GString(text) => 37i32.wrapping_add(string_hash(text)),
            Value::List(shared) => {
                let list = shared.borrow();
                if list.kind == ListKind::Array {
                    // Arrays keep the identity hash of the object.
                    return Rc::as_ptr(shared) as usize as i32;
                }
                list.items.iter().fold(1i32, |hash, item| {
                    hash.wrapping_mul(31).wrapping_add(item.java_hash())
                })
            }
            Value::Map(map) => map
                .borrow()
                .entries
                .iter()
                .fold(0i32, |hash, (key, value)| {
                    hash.wrapping_add(key.0.java_hash() ^ value.java_hash())
                }),
            Value::Set(set) => set
                .borrow()
                .items
                .iter()
                .fold(0i32, |hash, item| hash.wrapping_add(item.0.java_hash())),
            Value::MapEntry(entry) => entry.0.java_hash() ^ entry.1.java_hash(),
            // Groovy's `@EqualsAndHashCode` over the one field; checked against Nextflow.
            Value::MemoryUnit(count) | Value::Duration(count) => {
                (59 * 127i32).wrapping_add(long_hash(*count))
            }
            // Other objects keep the identity hash, which only needs to be consistent.
            Value::Closure(closure) => Rc::as_ptr(closure) as usize as i32,
            Value::Exception(exception) => Rc::as_ptr(exception) as usize as i32,
            Value::Pattern(pattern) => Rc::as_ptr(pattern) as usize as i32,
            Value::Matcher(matcher) => Rc::as_ptr(matcher) as usize as i32,
        }
    }
}

fn number_equals(left: &Number, right: &Number) -> bool {
    match (left, right) {
        (Number::Integer(a), Number::Integer(b)) => a == b,
        (Number::Long(a), Number::Long(b)) => a == b,
        (Number::BigInteger(a), Number::BigInteger(b)) => a == b,
        (Number::BigDecimal(a), Number::BigDecimal(b)) => a == b,
        (Number::Double(a), Number::Double(b)) => double_bits(*a) == double_bits(*b),
        (Number::Float(a), Number::Float(b)) => float_bits(*a) == float_bits(*b),
        _ => false,
    }
}

fn number_hash(number: &Number) -> i32 {
    match number {
        Number::Integer(value) => *value,
        Number::Long(value) => long_hash(*value),
        Number::BigInteger(value) => bigint_hash(value),
        Number::BigDecimal(value) => 31i32
            .wrapping_mul(bigint_hash(value.unscaled()))
            .wrapping_add(value.scale()),
        Number::Double(value) => long_hash(double_bits(*value)),
        Number::Float(value) => float_bits(*value),
    }
}

/// Java's `Double.doubleToLongBits`, with one canonical `NaN`.
fn double_bits(value: f64) -> i64 {
    if value.is_nan() {
        0x7ff8_0000_0000_0000
    } else {
        value.to_bits() as i64
    }
}

/// Java's `Float.floatToIntBits`, with one canonical `NaN`.
fn float_bits(value: f32) -> i32 {
    if value.is_nan() {
        0x7fc0_0000
    } else {
        value.to_bits() as i32
    }
}

fn long_hash(value: i64) -> i32 {
    (value ^ ((value as u64) >> 32) as i64) as i32
}

/// Java's `String.hashCode`, over UTF-16 code units.
fn string_hash(text: &str) -> i32 {
    text.encode_utf16().fold(0i32, |hash, unit| {
        hash.wrapping_mul(31).wrapping_add(i32::from(unit))
    })
}

/// Java's `BigInteger.hashCode`: the 32-bit words of the magnitude, most significant first,
/// then the sign.
fn bigint_hash(value: &BigInt) -> i32 {
    let (sign, words) = value.to_u32_digits();
    let hash = words.iter().rev().fold(0i32, |hash, word| {
        hash.wrapping_mul(31).wrapping_add(*word as i32)
    });
    if sign == Sign::Minus {
        hash.wrapping_neg()
    } else {
        hash
    }
}
