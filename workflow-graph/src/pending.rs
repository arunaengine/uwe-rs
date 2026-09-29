//! The attempts that started but that the driver has not heard of yet.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::time::SystemTime;

use crate::{ExecuteError, Order, Outcome, Task, TaskId};

/// An attempt that started, and its end once it came.
pub(crate) struct Slot {
    pub(crate) task: Arc<Task>,
    pub(crate) attempt: u32,
    pub(crate) started: SystemTime,
    pub(crate) end: Option<End>,
}

/// How an attempt ended, and when.
pub(crate) struct End {
    pub(crate) result: Result<Outcome, ExecuteError>,
    pub(crate) time: SystemTime,
    /// The how-manyth end this was, for the order in which attempts end.
    arrival: u64,
}

/// What [`Pending::take`] found.
pub(crate) enum Take {
    /// No attempt is pending.
    Empty,
    /// The attempt that finishes next has not ended yet.
    Wait,
    Ended(Box<Slot>),
}

/// Attempts in the order they started, handed out in the order of the run options.
pub(crate) struct Pending {
    slots: BTreeMap<u64, Slot>,
    positions: HashMap<(TaskId, u32), u64>,
    ended: BTreeSet<u64>,
    arrived: BTreeSet<(u64, u64)>,
    starts: u64,
    order: Order,
    random: SplitMix64,
    arrivals: u64,
    /// The attempt that the order chose to finish next, kept while the engine waits for it, so
    /// that the choice does not depend on when attempts end.
    chosen: Option<u64>,
}

impl Pending {
    pub(crate) fn new(order: Order) -> Self {
        let seed = match order {
            Order::Shuffled { seed } => seed,
            Order::AsAsked | Order::Reversed | Order::Finished => 0,
        };
        Pending {
            slots: BTreeMap::new(),
            positions: HashMap::new(),
            ended: BTreeSet::new(),
            arrived: BTreeSet::new(),
            starts: 0,
            order,
            random: SplitMix64(seed),
            arrivals: 0,
            chosen: None,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.slots.len()
    }

    /// Whether an attempt has not ended yet.
    pub(crate) fn running(&self) -> bool {
        self.slots.len() != self.ended.len()
    }

    pub(crate) fn start(&mut self, task: Arc<Task>, attempt: u32) {
        let index = self.starts;
        self.starts += 1;
        self.positions.insert((task.id, attempt), index);
        self.slots.insert(
            index,
            Slot {
                task,
                attempt,
                started: SystemTime::now(),
                end: None,
            },
        );
    }

    /// Notes how an attempt ended.
    pub(crate) fn end(
        &mut self,
        id: crate::TaskId,
        attempt: u32,
        result: Result<Outcome, ExecuteError>,
    ) {
        let Some(&index) = self.positions.get(&(id, attempt)) else {
            return;
        };
        let slot = self.slots.get_mut(&index).filter(|slot| slot.end.is_none());
        if let Some(slot) = slot {
            self.arrivals += 1;
            slot.end = Some(End {
                result,
                time: SystemTime::now(),
                arrival: self.arrivals,
            });
            self.ended.insert(index);
            self.arrived.insert((self.arrivals, index));
        }
    }

    pub(crate) fn cancel_remaining(&mut self) {
        for (&index, slot) in self.slots.iter_mut().filter(|(_, slot)| slot.end.is_none()) {
            self.arrivals += 1;
            slot.end = Some(End {
                result: Err(ExecuteError::Cancelled),
                time: SystemTime::now(),
                arrival: self.arrivals,
            });
            self.ended.insert(index);
            self.arrived.insert((self.arrivals, index));
        }
    }

    /// Takes any attempt that has ended, whatever the order, as when the run stopped.
    pub(crate) fn take_any(&mut self) -> Take {
        match self.ended.first().copied() {
            Some(index) => self.take_index(index),
            None => Take::Empty,
        }
    }

    /// Takes the attempt that finishes next, once it has ended.
    pub(crate) fn take(&mut self) -> Take {
        if self.slots.is_empty() {
            return Take::Empty;
        }
        if self.order == Order::Finished {
            return match self.arrived.first().copied() {
                Some((_, index)) => self.take_index(index),
                None => Take::Wait,
            };
        }
        let chosen = self.chosen.filter(|index| self.slots.contains_key(index));
        let index = match chosen {
            Some(index) => index,
            None => {
                let index = match self.order {
                    Order::AsAsked | Order::Finished => *self.slots.first_key_value().unwrap().0,
                    Order::Reversed => *self.slots.last_key_value().unwrap().0,
                    Order::Shuffled { .. } => *self
                        .slots
                        .keys()
                        .nth(self.random.below(self.slots.len()))
                        .unwrap(),
                };
                self.chosen = Some(index);
                index
            }
        };
        if self.slots[&index].end.is_none() {
            return Take::Wait;
        }
        self.chosen = None;
        self.take_index(index)
    }

    fn take_index(&mut self, index: u64) -> Take {
        let slot = self.slots.remove(&index).expect("a pending attempt");
        self.positions.remove(&(slot.task.id, slot.attempt));
        self.ended.remove(&index);
        if let Some(end) = &slot.end {
            self.arrived.remove(&(end.arrival, index));
        }
        Take::Ended(Box::new(slot))
    }
}

/// SplitMix64, a small random number generator. It is enough to pick pending tasks, and the
/// same seed always gives the same numbers.
struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A number below `bound`, which must not be zero.
    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TaskId;

    #[test]
    fn matches_splitmix64_reference() {
        // The first outputs for seed 0 of the reference implementation by Sebastiano Vigna.
        let mut random = SplitMix64(0);
        assert_eq!(random.next(), 0xe220_a839_7b1d_cdaf);
        assert_eq!(random.next(), 0x6e78_9e6a_a1b9_65f4);
    }

    fn task(id: u64) -> Arc<Task> {
        Arc::new(Task {
            id: TaskId(id),
            ..Task::default()
        })
    }

    fn ok(id: u64) -> Result<Outcome, ExecuteError> {
        Ok(Outcome {
            id: TaskId(id),
            ..Outcome::default()
        })
    }

    fn taken(take: Take) -> Option<u64> {
        match take {
            Take::Ended(slot) => Some(slot.task.id.0),
            Take::Empty | Take::Wait => None,
        }
    }

    #[test]
    fn waits_chosen_attempt() {
        let mut pending = Pending::new(Order::AsAsked);
        pending.start(task(1), 1);
        pending.start(task(2), 1);
        pending.end(TaskId(2), 1, ok(2));
        // The first to start finishes first, so the engine waits for it.
        assert!(matches!(pending.take(), Take::Wait));
        pending.end(TaskId(1), 1, ok(1));
        assert_eq!(taken(pending.take()), Some(1));
        assert_eq!(taken(pending.take()), Some(2));
        assert!(matches!(pending.take(), Take::Empty));
    }

    #[test]
    fn orders_finished_attempts() {
        let mut pending = Pending::new(Order::Finished);
        pending.start(task(1), 1);
        pending.start(task(2), 1);
        assert!(matches!(pending.take(), Take::Wait));
        pending.end(TaskId(2), 1, ok(2));
        pending.end(TaskId(1), 1, ok(1));
        assert_eq!(taken(pending.take()), Some(2));
        assert_eq!(taken(pending.take()), Some(1));
    }

    #[test]
    fn cancels_unfinished_attempts() {
        let mut pending = Pending::new(Order::Finished);
        pending.start(task(1), 1);
        pending.start(task(2), 1);
        pending.end(TaskId(2), 1, ok(2));
        pending.cancel_remaining();
        assert!(!pending.running());
        let Take::Ended(first) = pending.take_any() else {
            panic!("terminal attempt")
        };
        assert!(matches!(
            first.end.unwrap().result,
            Err(ExecuteError::Cancelled)
        ));
        let Take::Ended(second) = pending.take_any() else {
            panic!("terminal attempt")
        };
        assert!(second.end.unwrap().result.is_ok());
        assert!(matches!(pending.take_any(), Take::Empty));
    }

    #[derive(Debug, PartialEq)]
    enum Probe {
        Empty,
        Wait,
        Ended(u64, u32),
    }

    fn probe(take: Take) -> Probe {
        match take {
            Take::Empty => Probe::Empty,
            Take::Wait => Probe::Wait,
            Take::Ended(slot) => Probe::Ended(slot.task.id.0, slot.attempt),
        }
    }

    struct Reference {
        slots: Vec<(u64, u32, Option<u64>)>,
        order: Order,
        chosen: Option<(u64, u32)>,
        random: SplitMix64,
        arrivals: u64,
    }

    impl Reference {
        fn new(order: Order) -> Self {
            let seed = match order {
                Order::Shuffled { seed } => seed,
                _ => 0,
            };
            Self {
                slots: Vec::new(),
                order,
                chosen: None,
                random: SplitMix64(seed),
                arrivals: 0,
            }
        }

        fn end(&mut self, id: u64, attempt: u32) {
            if let Some(slot) = self
                .slots
                .iter_mut()
                .find(|slot| slot.0 == id && slot.1 == attempt && slot.2.is_none())
            {
                self.arrivals += 1;
                slot.2 = Some(self.arrivals);
            }
        }

        fn take(&mut self, any: bool) -> Probe {
            if self.slots.is_empty() {
                return Probe::Empty;
            }
            let index = if any {
                self.slots.iter().position(|slot| slot.2.is_some())
            } else if self.order == Order::Finished {
                self.slots
                    .iter()
                    .enumerate()
                    .filter_map(|(index, slot)| Some((slot.2?, index)))
                    .min()
                    .map(|(_, index)| index)
            } else {
                let index = self
                    .chosen
                    .and_then(|chosen| {
                        self.slots
                            .iter()
                            .position(|slot| (slot.0, slot.1) == chosen)
                    })
                    .unwrap_or_else(|| {
                        let index = match self.order {
                            Order::AsAsked | Order::Finished => 0,
                            Order::Reversed => self.slots.len() - 1,
                            Order::Shuffled { .. } => self.random.below(self.slots.len()),
                        };
                        self.chosen = Some((self.slots[index].0, self.slots[index].1));
                        index
                    });
                if self.slots[index].2.is_none() {
                    return Probe::Wait;
                }
                self.chosen = None;
                Some(index)
            };
            match index {
                Some(index) => {
                    let (id, attempt, _) = self.slots.remove(index);
                    Probe::Ended(id, attempt)
                }
                None if any => Probe::Empty,
                None => Probe::Wait,
            }
        }
    }

    #[test]
    fn preserves_indexed_orders() {
        let orders = [
            Order::AsAsked,
            Order::Reversed,
            Order::Finished,
            Order::Shuffled { seed: 0 },
            Order::Shuffled { seed: 42 },
            Order::Shuffled { seed: u64::MAX },
        ];
        for order in orders {
            let mut pending = Pending::new(order);
            let mut reference = Reference::new(order);
            for id in 1..=32 {
                pending.start(task(id), 1);
                reference.slots.push((id, 1, None));
            }
            for turn in 0..128 {
                let id = 1 + (turn * 17) % 32;
                let attempt = if turn % 3 == 0 { 2 } else { 1 };
                pending.end(TaskId(id), attempt, ok(id));
                reference.end(id, attempt);
                // A stale or duplicate notification must leave the first terminal result intact.
                pending.end(TaskId(id), attempt, Err(ExecuteError::Cancelled));
                reference.end(id, attempt);
                if let Some(index) = pending.positions.get(&(TaskId(id), attempt)) {
                    assert!(pending.slots[index].end.as_ref().unwrap().result.is_ok());
                }
                let any = turn % 11 == 0;
                let actual = probe(if any {
                    pending.take_any()
                } else {
                    pending.take()
                });
                let expected = reference.take(any);
                assert_eq!(actual, expected, "{order:?}, turn {turn}");
                if let Probe::Ended(id, 1) = expected {
                    pending.start(task(id), 2);
                    reference.slots.push((id, 2, None));
                }
                assert_eq!(pending.len(), reference.slots.len());
                assert_eq!(
                    pending.running(),
                    reference.slots.iter().any(|slot| slot.2.is_none())
                );
                assert_eq!(pending.positions.len(), pending.len());
                assert_eq!(pending.arrived.len(), pending.ended.len());
            }
            pending.cancel_remaining();
            for (id, attempt, _) in reference.slots.clone() {
                reference.end(id, attempt);
            }
            assert!(!pending.running());
            loop {
                let expected = reference.take(false);
                assert_eq!(probe(pending.take()), expected, "{order:?}, final drain");
                if expected == Probe::Empty {
                    break;
                }
            }
            assert!(pending.positions.is_empty());
            assert!(pending.ended.is_empty());
            assert!(pending.arrived.is_empty());
        }
    }

    #[test]
    fn cleanup_preserves_order() {
        let mut pending = Pending::new(Order::Shuffled { seed: 42 });
        for id in 1..=4 {
            pending.start(task(id), 1);
        }
        assert!(matches!(pending.take(), Take::Wait));
        pending.end(TaskId(4), 1, ok(4));
        pending.end(TaskId(2), 1, ok(2));
        assert_eq!(probe(pending.take_any()), Probe::Ended(2, 1));
        pending.start(task(2), 2);
        pending.end(TaskId(2), 1, ok(2));
        assert_eq!(pending.ended.len(), 1);
        pending.cancel_remaining();
        let actual: Vec<_> = std::iter::from_fn(|| match probe(pending.take_any()) {
            Probe::Ended(id, attempt) => Some((id, attempt)),
            _ => None,
        })
        .collect();
        assert_eq!(actual, [(1, 1), (3, 1), (4, 1), (2, 2)]);
        assert!(!pending.running());
        assert_eq!(pending.len(), 0);
    }
}
