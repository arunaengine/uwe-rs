//! The CPUs and memory that local tasks share. A task starts when its share is free. Tasks that
//! wait take their shares one after another: the one with the highest priority first, and of
//! equal ones the one that asked first.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cmp::Reverse;
use std::collections::BTreeSet;
use std::sync::{Mutex, PoisonError};

use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

const MIB: u64 = 1024 * 1024;

/// The CPUs and memory of this machine, or the part of them that tasks may use.
#[derive(Debug)]
pub struct Pool {
    total_cpus: u32,
    total_memory: u64,
    /// What is free, and the tasks that wait for their share.
    state: Mutex<State>,
    /// Wakes the waiting tasks when a share comes free or a task leaves the queue.
    changed: Notify,
    /// Fires when the run starts no more tasks, so that the tasks that wait give up.
    held: CancellationToken,
}

/// The place of a task in the queue: its priority, the highest first, then when it came.
type Ticket = (Reverse<u64>, u64);

#[derive(Debug)]
struct State {
    free_cpus: u32,
    /// In MiB.
    free_memory: u64,
    /// The tasks that wait, the next one first. Only the first may take its share, so that a
    /// task that needs much is not passed forever by tasks that need little.
    waiting: BTreeSet<Ticket>,
    /// Counts the tasks that came, so that each ticket is new.
    came: u64,
}

/// A task in the queue; it leaves the queue when this is dropped, also when the task stops
/// waiting because it was cancelled.
struct Place<'a> {
    pool: &'a Pool,
    ticket: Ticket,
}

impl Drop for Place<'_> {
    fn drop(&mut self) {
        self.pool.state().waiting.remove(&self.ticket);
        self.pool.changed.notify_waiters();
    }
}

/// A task's share of the pool, given back when it is dropped.
#[derive(Debug)]
pub struct Share<'a> {
    pool: &'a Pool,
    /// The CPUs and bytes of memory that the task may use: what it asked for, but at most all
    /// of the pool.
    pub cpus: u32,
    pub memory: Option<u64>,
}

impl Drop for Share<'_> {
    fn drop(&mut self) {
        let mut state = self.pool.state();
        state.free_cpus += self.cpus;
        state.free_memory += self.memory.map_or(0, mebibytes);
        drop(state);
        self.pool.changed.notify_waiters();
    }
}

impl Pool {
    /// A pool of `cpus` CPUs and `memory` bytes; both are at least 1 CPU and 1 MiB.
    pub fn new(cpus: u32, memory: u64) -> Pool {
        let total_cpus = cpus.max(1);
        let total_memory = memory.max(MIB);
        Pool {
            total_cpus,
            total_memory,
            state: Mutex::new(State {
                free_cpus: total_cpus,
                free_memory: mebibytes(total_memory),
                waiting: BTreeSet::new(),
                came: 0,
            }),
            changed: Notify::new(),
            held: CancellationToken::new(),
        }
    }

    /// Gives no more shares: the tasks that wait for theirs, and those that come later, wait
    /// for `held` instead, as a run that ends after a failure starts no more tasks.
    pub fn hold(&self) {
        self.held.cancel();
    }

    /// Ends when the pool is held.
    pub async fn held(&self) {
        self.held.cancelled().await
    }

    /// All CPUs and all memory of this machine.
    pub fn of_this_machine() -> Pool {
        let cpus = std::thread::available_parallelism().map_or(1, |cpus| cpus.get() as u32);
        Pool::new(cpus, total_memory().unwrap_or(u64::MAX / 2))
    }

    pub fn cpus(&self) -> u32 {
        self.total_cpus
    }

    pub fn memory(&self) -> u64 {
        self.total_memory
    }

    /// Waits until a task's share is free.
    /// See `docs/design/engine.md` for the full contract.
    pub async fn share(&self, cpus: u32, memory: Option<u64>, priority: u64) -> Share<'_> {
        let cpus = cpus.clamp(1, self.total_cpus);
        let memory = memory.map(|bytes| bytes.clamp(MIB, self.total_memory));
        let needed = memory.map_or(0, mebibytes);
        let place = self.enqueue(priority);
        loop {
            // Listening starts before the check, so that no change is missed between the two.
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let mut state = self.state();
                let first = state.waiting.first() == Some(&place.ticket);
                if first && state.free_cpus >= cpus && state.free_memory >= needed {
                    state.free_cpus -= cpus;
                    state.free_memory -= needed;
                    // Leaving the queue lets the next task look at what is left.
                    drop(state);
                    drop(place);
                    return Share {
                        pool: self,
                        cpus,
                        memory,
                    };
                }
            }
            changed.await;
        }
    }

    /// Puts a task into the queue.
    fn enqueue(&self, priority: u64) -> Place<'_> {
        let mut state = self.state();
        let ticket = (Reverse(priority), state.came);
        state.came += 1;
        state.waiting.insert(ticket);
        Place { pool: self, ticket }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn mebibytes(bytes: u64) -> u64 {
    bytes.div_ceil(MIB)
}

/// The memory of this machine, from `/proc/meminfo` on Linux.
fn total_memory() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    let line = text.lines().find(|line| line.starts_with("MemTotal:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib * 1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn waits_resource_share() {
        let pool = Pool::new(4, 8 * 1024 * MIB);
        let first = pool.share(3, Some(2 * 1024 * MIB), 0).await;
        assert_eq!((first.cpus, first.memory), (3, Some(2 * 1024 * MIB)));
        // More than the pool has is all of it, once the first task is done.
        let waiting = pool.share(8, Some(100 * 1024 * MIB), 0);
        let timeout = std::time::Duration::from_millis(50);
        let mut waiting = Box::pin(waiting);
        assert!(tokio::time::timeout(timeout, &mut waiting).await.is_err());
        drop(first);
        let all = waiting.await;
        assert_eq!((all.cpus, all.memory), (4, Some(8 * 1024 * MIB)));
    }

    #[tokio::test]
    async fn prioritizes_longer_chains() {
        let pool = std::sync::Arc::new(Pool::new(1, 1024 * MIB));
        let first = pool.share(1, None, 0).await;
        let order = std::sync::Arc::new(Mutex::new(Vec::new()));
        let mut waiting = Vec::new();
        // They come in this order; the second of equal priority comes after the first.
        for (name, priority) in [("a", 1), ("b", 3), ("c", 2), ("d", 3)] {
            let (pool, order) = (pool.clone(), order.clone());
            waiting.push(tokio::spawn(async move {
                let share = pool.share(1, None, priority).await;
                order.lock().unwrap().push(name);
                drop(share);
            }));
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        // A task that stops waiting leaves the queue.
        let gone = pool.share(1, None, 9);
        let timeout = std::time::Duration::from_millis(20);
        assert!(tokio::time::timeout(timeout, gone).await.is_err());
        drop(first);
        for task in waiting {
            task.await.expect("the task ends");
        }
        assert_eq!(*order.lock().unwrap(), ["b", "d", "c", "a"]);
    }

    #[tokio::test]
    async fn cancels_waiting_tasks() {
        let pool = Pool::new(1, 1024 * MIB);
        let timeout = std::time::Duration::from_millis(20);
        assert!(tokio::time::timeout(timeout, pool.held()).await.is_err());
        pool.hold();
        assert!(tokio::time::timeout(timeout, pool.held()).await.is_ok());
    }
}
