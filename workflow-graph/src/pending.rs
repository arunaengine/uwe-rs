//! The outcomes of tasks that ran but that the driver has not heard of yet.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::{Order, Outcome};

/// Outcomes in the order their tasks started, handed out in the order of the run options.
pub(crate) struct Pending {
    outcomes: Vec<Outcome>,
    order: Order,
    random: SplitMix64,
}

impl Pending {
    pub(crate) fn new(order: Order) -> Self {
        let seed = match order {
            Order::Shuffled { seed } => seed,
            Order::AsAsked | Order::Reversed => 0,
        };
        Pending {
            outcomes: Vec::new(),
            order,
            random: SplitMix64(seed),
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.outcomes.len()
    }

    pub(crate) fn push(&mut self, outcome: Outcome) {
        self.outcomes.push(outcome);
    }

    /// Takes the outcome of the task that finishes next.
    pub(crate) fn take(&mut self) -> Option<Outcome> {
        if self.outcomes.is_empty() {
            return None;
        }
        let index = match self.order {
            Order::AsAsked => 0,
            Order::Reversed => self.outcomes.len() - 1,
            Order::Shuffled { .. } => self.random.below(self.outcomes.len()),
        };
        Some(self.outcomes.remove(index))
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

    #[test]
    fn splitmix64_gives_the_reference_numbers() {
        // The first outputs for seed 0 of the reference implementation by Sebastiano Vigna.
        let mut random = SplitMix64(0);
        assert_eq!(random.next(), 0xe220_a839_7b1d_cdaf);
        assert_eq!(random.next(), 0x6e78_9e6a_a1b9_65f4);
    }
}
