//! CPython's hash values of numbers, `None` and tuples, and the order in which CPython
//! iterates sets of such values.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use indexmap::IndexSet;
use num_bigint::{BigUint, Sign};
use num_traits::ToPrimitive;

use crate::set_table::Table;
use crate::value::{Key, Value};

/// CPython reduces numbers modulo the prime 2**61 - 1 (`_PyHASH_MODULUS`).
const BITS: u32 = 61;
const MODULUS: u64 = (1 << BITS) - 1;

type Members = Rc<RefCell<IndexSet<Key>>>;
/// A set made by a literal, with the hashes of its members at that time.
type Literal = (Weak<RefCell<IndexSet<Key>>>, Vec<i64>);

thread_local! {
    static LITERALS: RefCell<HashMap<usize, Literal>> = RefCell::new(HashMap::new());
}

/// CPython's `hash()` of a value whose hash is the same in every process: numbers, `None`
/// and tuples of them. CPython hashes text with a random seed, so text has none.
pub fn stable_hash(value: &Value) -> Option<i64> {
    match value.plain() {
        Value::None => Some(4_238_894_112),
        Value::Bool(value) => Some(i64::from(*value)),
        Value::Int(value) => Some(signed(value.unsigned_abs() % MODULUS, *value < 0)),
        Value::BigInt(value) => {
            let rest = (value.magnitude() % BigUint::from(MODULUS)).to_u64()?;
            Some(signed(rest, value.sign() == Sign::Minus))
        }
        Value::Float(value) => float_hash(*value),
        Value::Tuple(items) => tuple_hash(items),
        _ => None,
    }
}

fn signed(magnitude: u64, negative: bool) -> i64 {
    let hash = if negative {
        -(magnitude as i64)
    } else {
        magnitude as i64
    };
    if hash == -1 { -2 } else { hash }
}

/// `_Py_HashDouble`; NaN hashes by identity in CPython, so it has no stable hash.
fn float_hash(value: f64) -> Option<i64> {
    if value.is_nan() {
        return None;
    }
    if value.is_infinite() {
        return Some(if value > 0.0 { 314_159 } else { -314_159 });
    }
    let (mut mantissa, mut exponent) = frexp(value.abs());
    let mut hash: u64 = 0;
    while mantissa != 0.0 {
        hash = ((hash << 28) & MODULUS) | hash >> (BITS - 28);
        mantissa *= 268_435_456.0;
        exponent -= 28;
        let whole = mantissa as u64;
        mantissa -= whole as f64;
        hash += whole;
        if hash >= MODULUS {
            hash -= MODULUS;
        }
    }
    let shift = if exponent >= 0 {
        exponent % BITS as i32
    } else {
        BITS as i32 - 1 - ((-1 - exponent) % BITS as i32)
    } as u32;
    hash = ((hash << shift) & MODULUS) | hash >> (BITS - shift);
    Some(signed(hash, value < 0.0))
}

/// The mantissa in [0.5, 1) and the exponent of a positive finite number, as C's `frexp`.
fn frexp(value: f64) -> (f64, i32) {
    if value == 0.0 {
        return (0.0, 0);
    }
    let bits = value.to_bits();
    let raw = ((bits >> 52) & 0x7ff) as i32;
    if raw == 0 {
        let (mantissa, exponent) = frexp(value * 2f64.powi(64));
        return (mantissa, exponent - 64);
    }
    let mantissa = f64::from_bits((bits & !(0x7ff << 52)) | (1022 << 52));
    (mantissa, raw - 1022)
}

/// `tuplehash`, the xxHash-based hash of CPython 3.8 and later.
fn tuple_hash(items: &[Value]) -> Option<i64> {
    const PRIME_1: u64 = 11_400_714_785_074_694_791;
    const PRIME_2: u64 = 14_029_467_366_897_019_727;
    const PRIME_5: u64 = 2_870_177_450_012_600_261;
    let mut hash = PRIME_5;
    for item in items {
        let lane = stable_hash(item)? as u64;
        hash = hash.wrapping_add(lane.wrapping_mul(PRIME_2));
        hash = hash.rotate_left(31);
        hash = hash.wrapping_mul(PRIME_1);
    }
    hash = hash.wrapping_add(items.len() as u64 ^ (PRIME_5 ^ 3_527_539));
    if hash == u64::MAX {
        return Some(1_546_275_796);
    }
    Some(hash as i64)
}

/// Notes a set made by a literal of three or more constants: CPython builds a frozenset of
/// them when it compiles the code and copies that into a new set, which orders it otherwise.
pub fn note_literal(set: &Members) {
    let Some(hashes) = hashes(&set.borrow()) else {
        return;
    };
    LITERALS.with(|literals| {
        let mut literals = literals.borrow_mut();
        if literals.len() > 1024 {
            literals.retain(|_, (weak, _)| weak.strong_count() > 0);
        }
        literals.insert(Rc::as_ptr(set) as usize, (Rc::downgrade(set), hashes));
    });
}

/// The members of a set in the order CPython iterates them. That order depends on random
/// text hashes in CPython, so sets with text or other objects keep their insertion order.
pub fn set_order(set: &Members) -> Vec<Value> {
    let members = set.borrow();
    let Some(hashes) = hashes(&members) else {
        return members.iter().map(|key| key.0.clone()).collect();
    };
    let literal = LITERALS.with(|literals| {
        literals
            .borrow()
            .get(&(Rc::as_ptr(set) as usize))
            .is_some_and(|(weak, noted)| {
                weak.upgrade().is_some_and(|live| Rc::ptr_eq(&live, set)) && *noted == hashes
            })
    });
    let Some(mut table) = incremental(members.iter()) else {
        return members.iter().map(|key| key.0.clone()).collect();
    };
    if literal && let Some(constant) = incremental(table.keys()) {
        table = Table::new();
        table.merge(&constant);
    }
    table.keys().map(|key| key.0.clone()).collect()
}

/// The members of a frozenset in the order CPython iterates them.
pub fn frozen_order(set: &IndexSet<Key>) -> Vec<Value> {
    match incremental(set.iter()) {
        Some(table) => table.keys().map(|key| key.0.clone()).collect(),
        None => set.iter().map(|key| key.0.clone()).collect(),
    }
}

fn hashes(set: &IndexSet<Key>) -> Option<Vec<i64>> {
    set.iter().map(|key| stable_hash(&key.0)).collect()
}

/// The table of a set that starts empty and gets the members one by one.
fn incremental<'a>(members: impl Iterator<Item = &'a Key>) -> Option<Table> {
    let mut table = Table::new();
    for key in members {
        table.add(key.clone(), stable_hash(&key.0)?);
    }
    Some(table)
}
