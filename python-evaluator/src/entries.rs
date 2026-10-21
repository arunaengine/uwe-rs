//! The entries of a dict in insertion order, removed at once by leaving a gap that a later
//! compaction closes, as CPython's dict does.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::Cell;
use std::collections::HashMap;

use crate::value::{Key, Value};

/// The entries of a dict. A removed entry leaves a gap, so that a removal costs the same
/// however many entries come after it; the gaps go once they outnumber the entries.
#[derive(Default)]
pub struct Entries {
    slots: Vec<Option<(Key, Value)>>,
    places: HashMap<Key, usize>,
    /// The first slot that may hold an entry, so that the gaps at the front are not walked
    /// again and again.
    head: Cell<usize>,
}

impl Clone for Entries {
    fn clone(&self) -> Self {
        self.iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }
}

/// What `entry` finds for a key: its value, or the place for a new one.
pub enum Entry<'a> {
    Occupied(&'a mut Value),
    Vacant(&'a mut Entries, Key),
}

impl<'a> Entry<'a> {
    pub fn or_insert(self, value: Value) -> &'a mut Value {
        self.or_insert_with(|| value)
    }

    pub fn or_insert_with(self, make: impl FnOnce() -> Value) -> &'a mut Value {
        match self {
            Entry::Occupied(value) => value,
            Entry::Vacant(entries, key) => {
                let place = entries.slots.len();
                entries.insert(key, make());
                &mut entries.slots[place].as_mut().expect("the new entry").1
            }
        }
    }
}

impl Entries {
    pub fn new() -> Entries {
        Entries::default()
    }

    pub fn len(&self) -> usize {
        self.places.len()
    }

    pub fn is_empty(&self) -> bool {
        self.places.is_empty()
    }

    pub fn contains_key(&self, key: &Key) -> bool {
        self.places.contains_key(key)
    }

    pub fn get(&self, key: &Key) -> Option<&Value> {
        let place = *self.places.get(key)?;
        self.slots[place].as_ref().map(|(_, value)| value)
    }

    pub fn get_mut(&mut self, key: &Key) -> Option<&mut Value> {
        let place = *self.places.get(key)?;
        self.slots[place].as_mut().map(|(_, value)| value)
    }

    /// The key that the dict holds for `key`, which may be another value that equals it.
    pub fn get_key_value(&self, key: &Key) -> Option<(&Key, &Value)> {
        let place = *self.places.get(key)?;
        self.slots[place].as_ref().map(|(key, value)| (key, value))
    }

    /// Sets the value of a key; a new key goes to the end, a known one keeps its place and
    /// its key object. Gives the value that the key had.
    pub fn insert(&mut self, key: Key, value: Value) -> Option<Value> {
        if let Some(place) = self.places.get(&key) {
            let slot = self.slots[*place].as_mut().expect("a live entry");
            return Some(std::mem::replace(&mut slot.1, value));
        }
        self.places.insert(key.clone(), self.slots.len());
        self.slots.push(Some((key, value)));
        None
    }

    pub fn entry(&mut self, key: Key) -> Entry<'_> {
        match self.places.get(&key).copied() {
            Some(place) => Entry::Occupied(&mut self.slots[place].as_mut().expect("live").1),
            None => Entry::Vacant(self, key),
        }
    }

    /// Removes a key and keeps the order of the others, at once.
    pub fn shift_remove(&mut self, key: &Key) -> Option<Value> {
        self.shift_remove_entry(key).map(|(_, value)| value)
    }

    pub fn shift_remove_entry(&mut self, key: &Key) -> Option<(Key, Value)> {
        let place = self.places.remove(key)?;
        let removed = self.slots[place].take();
        self.compact();
        removed
    }

    /// Removes the last entry, as `dict.popitem()`.
    pub fn pop(&mut self) -> Option<(Key, Value)> {
        while let Some(slot) = self.slots.pop() {
            if let Some((key, value)) = slot {
                self.places.remove(&key);
                self.head.set(self.head.get().min(self.slots.len()));
                return Some((key, value));
            }
        }
        self.head.set(0);
        None
    }

    pub fn first(&self) -> Option<(&Key, &Value)> {
        let start = self.head.get().min(self.slots.len());
        let offset = self.slots[start..].iter().position(Option::is_some)?;
        self.head.set(start + offset);
        self.slots[start + offset]
            .as_ref()
            .map(|(key, value)| (key, value))
    }

    pub fn last(&self) -> Option<(&Key, &Value)> {
        (self.slots.iter().rev().flatten())
            .next()
            .map(|(key, value)| (key, value))
    }

    pub fn clear(&mut self) {
        self.slots.clear();
        self.places.clear();
        self.head.set(0);
    }

    pub fn iter(&self) -> impl DoubleEndedIterator<Item = (&Key, &Value)> {
        let start = self.head.get().min(self.slots.len());
        (self.slots[start..].iter().flatten()).map(|(key, value)| (key, value))
    }

    pub fn iter_mut(&mut self) -> impl DoubleEndedIterator<Item = (&Key, &mut Value)> {
        (self.slots.iter_mut().flatten()).map(|(key, value)| (&*key, value))
    }

    pub fn keys(&self) -> impl DoubleEndedIterator<Item = &Key> {
        self.iter().map(|(key, _)| key)
    }

    pub fn values(&self) -> impl DoubleEndedIterator<Item = &Value> {
        self.iter().map(|(_, value)| value)
    }

    pub fn values_mut(&mut self) -> impl DoubleEndedIterator<Item = &mut Value> {
        self.iter_mut().map(|(_, value)| value)
    }

    pub fn into_values(self) -> impl DoubleEndedIterator<Item = Value> {
        self.into_iter().map(|(_, value)| value)
    }

    /// Keeps the entries for which `keep` holds, in their order.
    pub fn retain(&mut self, mut keep: impl FnMut(&Key, &mut Value) -> bool) {
        for slot in &mut self.slots {
            if let Some((key, value)) = slot
                && !keep(key, value)
            {
                self.places.remove(key);
                *slot = None;
            }
        }
        self.compact();
    }

    /// Orders the entries by `compare`, as `sorted` keeps a dict that is made from them.
    pub fn sort_by(
        &mut self,
        mut compare: impl FnMut(&Key, &Value, &Key, &Value) -> std::cmp::Ordering,
    ) {
        let mut entries: Vec<(Key, Value)> = std::mem::take(&mut self.slots)
            .into_iter()
            .flatten()
            .collect();
        entries.sort_by(|(a, x), (b, y)| compare(a, x, b, y));
        *self = entries.into_iter().collect();
    }

    /// Closes the gaps once they outnumber the entries, which keeps removals at a constant
    /// cost on average.
    fn compact(&mut self) {
        let gaps = self.slots.len() - self.places.len();
        if gaps <= 32 || gaps <= self.places.len() {
            return;
        }
        let entries: Vec<(Key, Value)> = std::mem::take(&mut self.slots)
            .into_iter()
            .flatten()
            .collect();
        self.places.clear();
        self.head.set(0);
        self.extend(entries);
    }
}

impl Extend<(Key, Value)> for Entries {
    fn extend<T: IntoIterator<Item = (Key, Value)>>(&mut self, items: T) {
        for (key, value) in items {
            self.insert(key, value);
        }
    }
}

impl FromIterator<(Key, Value)> for Entries {
    fn from_iter<T: IntoIterator<Item = (Key, Value)>>(items: T) -> Self {
        let mut entries = Entries::new();
        entries.extend(items);
        entries
    }
}

impl<'a> IntoIterator for &'a Entries {
    type Item = (&'a Key, &'a Value);
    type IntoIter = Box<dyn DoubleEndedIterator<Item = (&'a Key, &'a Value)> + 'a>;

    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.iter())
    }
}

impl IntoIterator for Entries {
    type Item = (Key, Value);
    type IntoIter = std::iter::Flatten<std::vec::IntoIter<Option<(Key, Value)>>>;

    fn into_iter(self) -> Self::IntoIter {
        self.slots.into_iter().flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_insertion_order() {
        let key = |number: i64| Key(Value::Int(number));
        let mut entries: Entries = (0..100)
            .map(|number| (key(number), Value::Int(number)))
            .collect();
        for number in (0..100).filter(|number| number % 3 != 0) {
            assert!(entries.shift_remove(&key(number)).is_some());
        }
        entries.insert(key(1), Value::Int(1));
        let keys: Vec<i64> = entries.keys().filter_map(|key| key.0.as_int()).collect();
        let mut expected: Vec<i64> = (0..100).filter(|number| number % 3 == 0).collect();
        expected.push(1);
        assert_eq!(keys, expected);
        assert_eq!(entries.pop().map(|(key, _)| key.0.as_int()), Some(Some(1)));
        assert_eq!(
            entries.first().map(|(key, _)| key.0.as_int()),
            Some(Some(0))
        );
        assert_eq!(entries.len(), 34);
    }
}
