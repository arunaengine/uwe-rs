//! The hash table of a CPython set (`setobject.c`), whose layout decides the order in which
//! a set of numbers or tuples is iterated.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::hashing::stable_hash;
use crate::value::Key;

/// The probing constants of CPython's `setobject.c`.
const PERTURB_SHIFT: u32 = 5;
const LINEAR_PROBES: usize = 9;

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
}
