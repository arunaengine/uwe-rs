//! The hash table of a CPython set (`setobject.c`), whose layout decides the order in which
//! a set of numbers or tuples is iterated, and the set operations that fill it.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use indexmap::IndexSet;

use crate::hashing::stable_hash;
use crate::value::{Key, Value};

/// The probing constants of CPython's `setobject.c`.
const PERTURB_SHIFT: u32 = 5;
const LINEAR_PROBES: usize = 9;

/// An operand of a set operation: a set with its table, or the keys of a dict or the items
/// of another iterable, in their order.
pub enum Operand {
    Table(Table, usize),
    Dict(IndexSet<Key>),
    Items(IndexSet<Key>),
}

impl Operand {
    /// The operand of a value whose members, as the operation collected them, are `members`;
    /// `None` when one has no hash that is the same in every process.
    pub fn of(value: &Value, members: &IndexSet<Key>) -> Option<Operand> {
        // In their order, and found at once by `contains`.
        let keys = || members.clone();
        let operand = match value.plain() {
            Value::Set(_) | Value::FrozenSet(_) => Operand::Table(
                crate::hashing::synced(value)?,
                crate::hashing::address(value),
            ),
            Value::Dict(_) => Operand::Dict(keys()),
            _ => Operand::Items(keys()),
        };
        let stable = match &operand {
            Operand::Table(..) => true,
            Operand::Dict(keys) | Operand::Items(keys) => {
                keys.iter().all(|key| stable_hash(&key.0).is_some())
            }
        };
        stable.then_some(operand)
    }

    fn contains(&self, key: &Key) -> bool {
        match self {
            Operand::Table(table, _) => table.find(key).is_some(),
            Operand::Dict(keys) | Operand::Items(keys) => keys.contains(key),
        }
    }

    fn len(&self) -> usize {
        match self {
            Operand::Table(table, _) => table.used,
            Operand::Dict(keys) | Operand::Items(keys) => keys.len(),
        }
    }
}

/// The table of the new set that a method or an operator of `this` makes with `others`, as
/// `setobject.c` makes it: `copy`, `union`, `intersection`, `difference` or
/// `symmetric_difference`.
pub fn derived(name: &str, this: &Operand, others: &[Operand]) -> Option<Table> {
    let Operand::Table(table, address) = this else {
        return None;
    };
    let mut result = table.copied();
    match name {
        "intersection" if !others.is_empty() => result = intersected(table, *address, others),
        "copy" | "intersection" => {}
        "union" => {
            for other in others {
                if !matches!(other, Operand::Table(_, other) if other == address) {
                    result.update(other);
                }
            }
        }
        "difference" => {
            if let Some((first, rest)) = others.split_first() {
                result = table.difference(first, *address);
                for other in rest {
                    result.discard_all(other, usize::MAX);
                }
            }
        }
        "symmetric_difference" => {
            let other = others.first()?;
            result = Table::new();
            result.update(other);
            result.toggle_all(this, usize::MAX);
        }
        _ => return None,
    }
    Some(result)
}

/// The table of `target` after an in-place method with `others`: `update`,
/// `intersection_update`, `difference_update` or `symmetric_difference_update`.
pub fn updated(target: &Operand, name: &str, others: &[Operand]) -> Option<Table> {
    let Operand::Table(table, address) = target else {
        return None;
    };
    let mut result = table.clone();
    match name {
        "update" | "difference_update" => apply(&mut result, *address, name, others),
        "intersection_update" => {
            result = intersected(table, *address, others);
            result.finger = table.finger;
        }
        "symmetric_difference_update" => others
            .iter()
            .for_each(|other| result.toggle_all(other, *address)),
        _ => return None,
    }
    Some(result)
}

/// `update` or `difference_update` on the table of the set at `address`, where it is.
pub(crate) fn apply(table: &mut Table, address: usize, name: &str, others: &[Operand]) {
    for other in others {
        match name {
            "update" if !matches!(other, Operand::Table(_, other) if *other == address) => {
                table.update(other)
            }
            "difference_update" => table.discard_all(other, address),
            _ => {}
        }
    }
}

/// `set_intersection_multi`: each operand in turn with what is left, from the set itself.
fn intersected(table: &Table, address: usize, others: &[Operand]) -> Table {
    if others.is_empty() {
        return table.copied();
    }
    let (mut result, mut address) = (table.clone(), address);
    for other in others {
        result = result.intersection(other, address);
        address = usize::MAX;
    }
    result
}

/// The table of a new set made of an operand, as `set(...)` and `frozenset(...)` make it.
pub fn made(from: &Operand) -> Table {
    let mut table = Table::new();
    table.update(from);
    table
}

#[derive(Clone)]
enum Slot {
    Empty,
    Dummy,
    Used(i64, Key),
}

/// The hash table of a CPython set (`setobject.c`): where each member lies, which decides
/// the order in which the set is iterated.
#[derive(Clone)]
pub struct Table {
    slots: Vec<Slot>,
    /// The slots that are used or dummies, and the members.
    fill: usize,
    pub(crate) used: usize,
    /// Where the next `pop` starts to look.
    finger: usize,
}

impl Table {
    pub(crate) fn new() -> Table {
        Table {
            slots: vec![Slot::Empty; 8],
            fill: 0,
            used: 0,
            finger: 0,
        }
    }

    fn mask(&self) -> usize {
        self.slots.len() - 1
    }

    pub(crate) fn keys(&self) -> impl Iterator<Item = &Key> {
        self.entries().map(|(key, _)| key)
    }

    pub(crate) fn entries(&self) -> impl Iterator<Item = (&Key, i64)> {
        self.slots.iter().filter_map(|slot| match slot {
            Slot::Used(hash, key) => Some((key, *hash)),
            _ => None,
        })
    }

    /// The next slot of a probe sequence, as `i = (i * 5 + 1 + perturb) & mask`.
    fn next(slot: usize, perturb: &mut u64, mask: usize) -> usize {
        *perturb >>= PERTURB_SHIFT;
        (slot
            .wrapping_mul(5)
            .wrapping_add(1)
            .wrapping_add(*perturb as usize))
            & mask
    }

    /// `set_lookkey`: the slot of a member.
    pub(crate) fn find(&self, key: &Key) -> Option<usize> {
        let hash = stable_hash(&key.0)?;
        let mask = self.mask();
        let mut perturb = hash as u64;
        let mut slot = (hash as u64 as usize) & mask;
        loop {
            let probes = if slot + LINEAR_PROBES <= mask {
                LINEAR_PROBES
            } else {
                0
            };
            for (offset, entry) in self.slots[slot..=slot + probes].iter().enumerate() {
                match entry {
                    Slot::Empty => return None,
                    Slot::Used(other, member) if *other == hash && member == key => {
                        return Some(slot + offset);
                    }
                    _ => {}
                }
            }
            slot = Table::next(slot, &mut perturb, mask);
        }
    }

    /// `set_add_entry`: a new member goes to the first dummy of its probe sequence, or else
    /// to its first free slot, and the table grows once it is three fifths full.
    pub(crate) fn add(&mut self, key: Key, hash: i64) {
        let mask = self.mask();
        let mut perturb = hash as u64;
        let mut slot = (hash as u64 as usize) & mask;
        let mut dummy = None;
        loop {
            let probes = if slot + LINEAR_PROBES <= mask {
                LINEAR_PROBES
            } else {
                0
            };
            for index in slot..=slot + probes {
                match &self.slots[index] {
                    Slot::Empty => {
                        self.used += 1;
                        if let Some(dummy) = dummy {
                            self.slots[dummy] = Slot::Used(hash, key);
                            return;
                        }
                        self.slots[index] = Slot::Used(hash, key);
                        self.fill += 1;
                        if self.fill * 5 >= mask * 3 {
                            self.resize(match self.used > 50_000 {
                                true => self.used * 2,
                                false => self.used * 4,
                            });
                        }
                        return;
                    }
                    Slot::Used(other, member) if *other == hash && *member == key => return,
                    Slot::Dummy if dummy.is_none() => dummy = Some(index),
                    _ => {}
                }
            }
            slot = Table::next(slot, &mut perturb, mask);
        }
    }

    /// `set_insert_clean`: a member that the table surely lacks, into a table without dummies.
    fn insert_clean(&mut self, hash: i64, key: Key) {
        let mask = self.mask();
        let mut perturb = hash as u64;
        let mut slot = (hash as u64 as usize) & mask;
        loop {
            let probes = if slot + LINEAR_PROBES <= mask {
                LINEAR_PROBES
            } else {
                0
            };
            let free = self.slots[slot..=slot + probes]
                .iter()
                .position(|entry| matches!(entry, Slot::Empty));
            if let Some(offset) = free {
                self.slots[slot + offset] = Slot::Used(hash, key);
                return;
            }
            slot = Table::next(slot, &mut perturb, mask);
        }
    }

    /// `set_table_resize`: a table of the smallest power of two above `minused`, with the
    /// members in the order of the old table and no dummies.
    fn resize(&mut self, minused: usize) {
        let mut size = 8;
        while size <= minused {
            size <<= 1;
        }
        let old = std::mem::replace(&mut self.slots, vec![Slot::Empty; size]);
        self.fill = self.used;
        for slot in old {
            if let Slot::Used(hash, key) = slot {
                self.insert_clean(hash, key);
            }
        }
    }

    /// `set_discard_entry`: the member's slot becomes a dummy.
    pub(crate) fn discard(&mut self, key: &Key, hash: i64) -> bool {
        let _ = hash;
        let Some(index) = self.find(key) else {
            return false;
        };
        self.slots[index] = Slot::Dummy;
        self.used -= 1;
        true
    }

    pub(crate) fn clear(&mut self) {
        self.slots = vec![Slot::Empty; 8];
        self.fill = 0;
        self.used = 0;
    }

    /// `set_pop`.
    pub(crate) fn pop(&mut self) -> Option<Key> {
        if self.used == 0 {
            return None;
        }
        let mask = self.mask();
        let mut index = self.finger & mask;
        while !matches!(self.slots[index], Slot::Used(..)) {
            index = (index + 1) & mask;
        }
        let Slot::Used(_, key) = std::mem::replace(&mut self.slots[index], Slot::Dummy) else {
            return None;
        };
        self.used -= 1;
        self.finger = index + 1;
        Some(key)
    }

    /// `set_merge`: the members of another set into this one. An empty table of the same
    /// size takes the layout as it is; an empty one of another size gets them in table order.
    pub(crate) fn merge(&mut self, other: &Table) {
        if other.used == 0 {
            return;
        }
        if (self.fill + other.used) * 5 >= self.mask() * 3 {
            self.resize((self.used + other.used) * 2);
        }
        if self.fill == 0 && self.mask() == other.mask() && other.fill == other.used {
            for (index, slot) in other.slots.iter().enumerate() {
                if let Slot::Used(..) = slot {
                    self.slots[index] = slot.clone();
                }
            }
            self.fill = other.fill;
            self.used = other.used;
            return;
        }
        if self.fill == 0 {
            self.fill = other.used;
            self.used = other.used;
            for (key, hash) in other.entries() {
                self.insert_clean(hash, key.clone());
            }
            return;
        }
        for (key, hash) in other.entries() {
            self.add(key.clone(), hash);
        }
    }

    /// `set_copy`: a new set with the members of this one.
    fn copied(&self) -> Table {
        let mut table = Table::new();
        table.merge(self);
        table
    }

    /// `set_update_internal`.
    fn update(&mut self, other: &Operand) {
        match other {
            Operand::Table(table, _) => self.merge(table),
            Operand::Dict(keys) => {
                if (self.fill + keys.len()) * 5 >= self.mask() * 3 {
                    self.resize((self.used + keys.len()) * 2);
                }
                self.add_all(keys);
            }
            Operand::Items(keys) => self.add_all(keys),
        }
    }

    fn add_all(&mut self, keys: &IndexSet<Key>) {
        for key in keys {
            if let Some(hash) = stable_hash(&key.0) {
                self.add(key.clone(), hash);
            }
        }
    }

    /// The keys of an operand in the order that `setobject.c` walks them.
    fn walked(other: &Operand) -> Vec<(Key, i64)> {
        match other {
            Operand::Table(table, _) => (table.entries())
                .map(|(key, hash)| (key.clone(), hash))
                .collect(),
            Operand::Dict(keys) | Operand::Items(keys) => (keys.iter())
                .filter_map(|key| Some((key.clone(), stable_hash(&key.0)?)))
                .collect(),
        }
    }

    /// `set_intersection`: a new set that walks the smaller set and keeps what the other has.
    fn intersection(&self, other: &Operand, address: usize) -> Table {
        if matches!(other, Operand::Table(_, other) if *other == address) {
            return self.copied();
        }
        let mut result = Table::new();
        let this = Operand::Table(self.clone(), address);
        let (walked, kept) = match other {
            Operand::Table(table, _) if table.used > self.used => (&this, other),
            Operand::Table(..) => (other, &this),
            _ => (other, &this),
        };
        for (key, hash) in Table::walked(walked) {
            if kept.contains(&key) {
                result.add(key, hash);
            }
        }
        result
    }

    /// `set_difference`: a copy without the other's members when this set is much larger,
    /// else a new set of the members of this one, in table order, that the other lacks.
    fn difference(&self, other: &Operand, address: usize) -> Table {
        let small = matches!(other, Operand::Table(..) | Operand::Dict(_))
            && (self.used >> 2) <= other.len();
        if !small {
            let mut result = self.copied();
            result.discard_all(other, address);
            return result;
        }
        let mut result = Table::new();
        for (key, hash) in self.entries() {
            if !other.contains(key) {
                result.add(key.clone(), hash);
            }
        }
        result
    }

    /// `set_difference_update_internal`, which also drops the dummies once they are more
    /// than a quarter of the table.
    fn discard_all(&mut self, other: &Operand, address: usize) {
        if matches!(other, Operand::Table(_, other) if *other == address) {
            self.clear();
            return;
        }
        let walked = match other {
            Operand::Table(table, _) if (table.used >> 3) > self.used => {
                let this = Operand::Table(self.clone(), address);
                Table::walked(&Operand::Table(table.intersection(&this, usize::MAX), 0))
            }
            other => Table::walked(other),
        };
        for (key, hash) in walked {
            self.discard(&key, hash);
        }
        if self.fill - self.used > self.mask() / 4 {
            self.resize(match self.used > 50_000 {
                true => self.used * 2,
                false => self.used * 4,
            });
        }
    }

    /// `set_symmetric_difference_update`: walks the other as a set, and takes out what this
    /// one has and adds what it lacks.
    fn toggle_all(&mut self, other: &Operand, address: usize) {
        if matches!(other, Operand::Table(_, other) if *other == address) {
            self.clear();
            return;
        }
        let walked = match other {
            Operand::Items(_) => {
                let mut table = Table::new();
                table.update(other);
                Table::walked(&Operand::Table(table, 0))
            }
            other => Table::walked(other),
        };
        for (key, hash) in walked {
            if !self.discard(&key, hash) {
                self.add(key, hash);
            }
        }
    }
}
