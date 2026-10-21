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

use crate::set_table::{Operand, Table};
use crate::value::{Key, Value};

/// CPython reduces numbers modulo the prime 2**61 - 1 (`_PyHASH_MODULUS`).
const BITS: u32 = 61;
const MODULUS: u64 = (1 << BITS) - 1;
type Members = Rc<RefCell<IndexSet<Key>>>;

/// The set or frozenset that a table belongs to, while it lives.
enum Holder {
    Set(Weak<RefCell<IndexSet<Key>>>),
    Frozen(Weak<IndexSet<Key>>),
}

thread_local! {
    /// The tables of the sets whose layout is known, by the address of their members.
    static TABLES: RefCell<Registry> = RefCell::new(Registry::default());
}

#[derive(Default)]
struct Registry {
    tables: HashMap<usize, (Holder, Table)>,
    /// How many tables there may be before the ones of gone sets are dropped.
    limit: usize,
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

/// `hash()` of a key: the hash of an object's `__hash__`, CPython's hash of a number, or else
/// one of iunctus, as CPython hashes text with a random seed.
pub fn python_hash(value: &Value) -> i64 {
    use std::hash::{Hash, Hasher};
    if let Value::Instance(instance) = value.plain()
        && let Some((hash, _)) = instance.hashed.get()
    {
        return hash;
    }
    if let Some(hash) = stable_hash(value) {
        return hash;
    }
    // Below the modulus of numbers, so that an object's `__hash__` that gives it keeps it.
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    Key(value.plain().clone()).hash(&mut hasher);
    (hasher.finish() % MODULUS) as i64
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
    let Some(first) = incremental(set.borrow().iter()) else {
        return;
    };
    // The constant is made of the members in the order of a first set of them.
    let Some(constant) = incremental(first.keys()) else {
        return;
    };
    let mut table = Table::new();
    table.merge(&constant);
    register(&Value::Set(set.clone()), table);
}

/// The members of a set in the order CPython iterates them. That order depends on random
/// text hashes in CPython, so sets with text or other objects keep their insertion order.
pub fn set_order(set: &Members) -> Vec<Value> {
    match synced(&Value::Set(set.clone())) {
        Some(table) => table.keys().map(|key| key.0.clone()).collect(),
        None => set.borrow().iter().map(|key| key.0.clone()).collect(),
    }
}

/// The members of a frozenset in the order CPython iterates them.
pub fn frozen_order(set: &Rc<IndexSet<Key>>) -> Vec<Value> {
    match synced(&Value::FrozenSet(set.clone())) {
        Some(table) => table.keys().map(|key| key.0.clone()).collect(),
        None => set.iter().map(|key| key.0.clone()).collect(),
    }
}

/// The address of the members of a set or frozenset, which names its table.
pub(crate) fn address(value: &Value) -> usize {
    match value.plain() {
        Value::Set(set) => Rc::as_ptr(set) as *const u8 as usize,
        Value::FrozenSet(set) => Rc::as_ptr(set) as *const u8 as usize,
        _ => 0,
    }
}

/// Keeps the table of a set or frozenset, which its iteration follows from now on.
pub fn register(value: &Value, table: Table) {
    let holder = match value.plain() {
        Value::Set(set) => Holder::Set(Rc::downgrade(set)),
        Value::FrozenSet(set) => Holder::Frozen(Rc::downgrade(set)),
        _ => return,
    };
    TABLES.with(|registry| {
        let mut registry = registry.borrow_mut();
        if registry.tables.len() >= registry.limit.max(1024) {
            registry.tables.retain(|_, (holder, _)| holder.alive());
            registry.limit = registry.tables.len() * 2;
        }
        registry.tables.insert(address(value), (holder, table));
    });
}

/// Changes the table of a set as `set.add`, `set.discard` (`added` false) or `set.clear`
/// (no key) do, before its members change.
pub fn change(set: &Members, key: Option<&Key>, added: bool) {
    let hash = key.map(|key| stable_hash(&key.0));
    if let Some(None) = hash {
        forget(address(&Value::Set(set.clone())));
        return;
    }
    in_place(set, |table| match (key, hash.flatten()) {
        (Some(key), Some(hash)) if added => table.add(key.clone(), hash),
        (Some(key), Some(hash)) => {
            table.discard(key, hash);
        }
        _ => table.clear(),
    });
}

/// Applies `update` or `difference_update` to the table of a set where it is, before its
/// members change; a set without such operands keeps no table.
pub fn apply(set: &Members, name: &str, others: Option<&[Operand]>) {
    let address = address(&Value::Set(set.clone()));
    match others {
        Some(others) => {
            in_place(set, |table| {
                crate::set_table::apply(table, address, name, others)
            });
        }
        None => forget(address),
    }
}

fn forget(address: usize) {
    TABLES.with(|registry| registry.borrow_mut().tables.remove(&address));
}

/// The member that `set.pop()` takes: the first one of the table from where the last pop
/// stopped; the first one inserted when the set has no table.
pub fn pop(set: &Members) -> Option<Key> {
    let popped = in_place(set, Table::pop).flatten();
    match popped {
        Some(key) if set.borrow().contains(&key) => Some(key),
        // A table that is not the set's anymore is built again.
        Some(_) => {
            forget(address(&Value::Set(set.clone())));
            in_place(set, Table::pop).flatten()
        }
        None if set.borrow().is_empty() => None,
        None => set.borrow().first().cloned(),
    }
}

/// Applies a change to the kept table of a set where it is, which a run of many changes
/// needs to stay fast; a set without a table, or with one of another size, gets one first.
fn in_place<R>(set: &Members, apply: impl FnOnce(&mut Table) -> R) -> Option<R> {
    let value = Value::Set(set.clone());
    let address = address(&value);
    let length = set.borrow().len();
    let mut apply = Some(apply);
    let done = TABLES.with(|registry| {
        let mut registry = registry.borrow_mut();
        let (holder, table) = registry.tables.get_mut(&address)?;
        let current = holder.owns(address) && table.used == length;
        current.then(|| apply.take().map(|apply| apply(table)))?
    });
    if done.is_some() {
        return done;
    }
    let mut table = synced(&value)?;
    let result = (apply.take()?)(&mut table);
    register(&value, table);
    Some(result)
}

impl Holder {
    fn alive(&self) -> bool {
        match self {
            Holder::Set(weak) => weak.strong_count() > 0,
            Holder::Frozen(weak) => weak.strong_count() > 0,
        }
    }

    /// Whether the set at `address` is the one that the table was kept for.
    fn owns(&self, address: usize) -> bool {
        match self {
            Holder::Set(weak) => {
                (weak.upgrade()).is_some_and(|set| self::address(&Value::Set(set)) == address)
            }
            Holder::Frozen(weak) => {
                (weak.upgrade()).is_some_and(|set| self::address(&Value::FrozenSet(set)) == address)
            }
        }
    }
}

/// The table of a set as it is now: the kept one with the members that came or went since,
/// or one built by adding the members in their order. `None` for members without a hash
/// that is the same in every process.
pub(crate) fn synced(value: &Value) -> Option<Table> {
    let members = match value.plain() {
        Value::Set(set) => set.borrow().clone(),
        Value::FrozenSet(set) => (**set).clone(),
        _ => return None,
    };
    let address = address(value);
    let kept = TABLES.with(|registry| {
        let registry = registry.borrow();
        let (holder, table) = registry.tables.get(&address)?;
        holder.owns(address).then(|| table.clone())
    });
    let table = match kept {
        Some(mut table) => {
            let gone: Vec<(Key, i64)> = table
                .entries()
                .filter(|(key, _)| !members.contains(*key))
                .map(|(key, hash)| (key.clone(), hash))
                .collect();
            for (key, hash) in gone {
                table.discard(&key, hash);
            }
            for key in &members {
                if table.find(key).is_none() {
                    table.add(key.clone(), stable_hash(&key.0)?);
                }
            }
            table
        }
        None => incremental(members.iter())?,
    };
    register(value, table.clone());
    Some(table)
}

/// The table of a set that starts empty and gets the members one by one.
fn incremental<'a>(members: impl Iterator<Item = &'a Key>) -> Option<Table> {
    let mut table = Table::new();
    for key in members {
        table.add(key.clone(), stable_hash(&key.0)?);
    }
    Some(table)
}
