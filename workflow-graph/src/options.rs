//! How the engine runs tasks: how many may be pending at once, and in which order they finish.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

/// Options of a run. The default runs one task at a time in the order the driver asked for
/// them, as [`execute`](crate::execute) does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunOptions {
    /// How many tasks may be pending at once: started, but the driver has not heard of their
    /// outcome yet. Zero counts as one.
    pub max_pending: usize,
    /// The order in which pending tasks finish.
    pub order: Order,
}

impl Default for RunOptions {
    fn default() -> Self {
        RunOptions {
            max_pending: 1,
            order: Order::AsAsked,
        }
    }
}

/// The order in which pending tasks finish. With several tasks pending, an order other than
/// the one asked for shows whether a driver depends on the order of outcomes, as it must not
/// once tasks run in parallel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Order {
    /// The task that started first finishes first.
    #[default]
    AsAsked,
    /// The task that started last finishes first.
    Reversed,
    /// A random pending task finishes first. The same seed gives the same order again.
    Shuffled { seed: u64 },
    /// Tasks finish when their attempts end, as real executors run them. Unlike the other
    /// orders, it depends on how long tasks take.
    Finished,
}
