//! The hash table of a CPython set (`setobject.c`), whose layout decides the order in which
//! a set of numbers or tuples is iterated.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::value::Key;

/// The probing constants of CPython's `setobject.c`.
const PERTURB_SHIFT: u32 = 5;
const LINEAR_PROBES: usize = 9;

#[derive(Clone)]
enum Slot {
    Empty,
    Used(i64, Key),
}

/// The hash table of a CPython set (`setobject.c`): where each member lies, which decides
/// the order in which the set is iterated.
#[derive(Clone)]
pub struct Table {
    slots: Vec<Slot>,
    /// The slots that are used, and the members.
    fill: usize,
    used: usize,
}

impl Table {
    pub(crate) fn new() -> Table {
        Table {
            slots: vec![Slot::Empty; 8],
            fill: 0,
            used: 0,
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

    /// `set_add_entry`: a new member goes to its first free slot, and the table grows once
    /// it is three fifths full.
    pub(crate) fn add(&mut self, key: Key, hash: i64) {
        let mask = self.mask();
        let mut perturb = hash as u64;
        let mut slot = (hash as u64 as usize) & mask;
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
