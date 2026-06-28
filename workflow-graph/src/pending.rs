//! The attempts that started but that the driver has not heard of yet.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::sync::Arc;
use std::time::SystemTime;

use crate::{ExecuteError, Order, Outcome, Task};

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
    Ended(Slot),
}

/// Attempts in the order they started, handed out in the order of the run options.
pub(crate) struct Pending {
    slots: Vec<Slot>,
    order: Order,
    random: SplitMix64,
    arrivals: u64,
    /// The attempt that the order chose to finish next, kept while the engine waits for it, so
    /// that the choice does not depend on when attempts end.
    chosen: Option<(Arc<Task>, u32)>,
}

impl Pending {
    pub(crate) fn new(order: Order) -> Self {
        let seed = match order {
            Order::Shuffled { seed } => seed,
            Order::AsAsked | Order::Reversed | Order::Finished => 0,
        };
        Pending {
            slots: Vec::new(),
            order,
            random: SplitMix64(seed),
            arrivals: 0,
            chosen: None,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.slots.len()
    }

    pub(crate) fn start(&mut self, task: Arc<Task>, attempt: u32) {
        self.slots.push(Slot {
            task,
            attempt,
            started: SystemTime::now(),
            end: None,
        });
    }

    /// Notes how an attempt ended.
    pub(crate) fn end(
        &mut self,
        id: crate::TaskId,
        attempt: u32,
        result: Result<Outcome, ExecuteError>,
    ) {
        let slot = (self.slots.iter_mut())
            .find(|slot| slot.task.id == id && slot.attempt == attempt && slot.end.is_none());
        if let Some(slot) = slot {
            self.arrivals += 1;
            slot.end = Some(End {
                result,
                time: SystemTime::now(),
                arrival: self.arrivals,
            });
        }
    }

    /// Takes the attempt that finishes next, once it has ended.
    pub(crate) fn take(&mut self) -> Take {
        if self.slots.is_empty() {
            return Take::Empty;
        }
        if self.order == Order::Finished {
            let first = (self.slots.iter().enumerate())
                .filter_map(|(index, slot)| Some((slot.end.as_ref()?.arrival, index)))
                .min();
            return match first {
                Some((_, index)) => Take::Ended(self.slots.remove(index)),
                None => Take::Wait,
            };
        }
        let chosen = match &self.chosen {
            Some((task, attempt)) => self
                .slots
                .iter()
                .position(|slot| Arc::ptr_eq(&slot.task, task) && slot.attempt == *attempt),
            None => None,
        };
        let index = match chosen {
            Some(index) => index,
            None => {
                let index = match self.order {
                    Order::AsAsked | Order::Finished => 0,
                    Order::Reversed => self.slots.len() - 1,
                    Order::Shuffled { .. } => self.random.below(self.slots.len()),
                };
                let slot = &self.slots[index];
                self.chosen = Some((slot.task.clone(), slot.attempt));
                index
            }
        };
        if self.slots[index].end.is_none() {
            return Take::Wait;
        }
        self.chosen = None;
        Take::Ended(self.slots.remove(index))
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
    fn splitmix64_gives_the_reference_numbers() {
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
    fn orders_wait_for_the_attempt_they_chose() {
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
    fn the_finished_order_takes_attempts_as_they_end() {
        let mut pending = Pending::new(Order::Finished);
        pending.start(task(1), 1);
        pending.start(task(2), 1);
        assert!(matches!(pending.take(), Take::Wait));
        pending.end(TaskId(2), 1, ok(2));
        pending.end(TaskId(1), 1, ok(1));
        assert_eq!(taken(pending.take()), Some(2));
        assert_eq!(taken(pending.take()), Some(1));
    }
}
