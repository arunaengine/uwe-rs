//! The CPUs and memory that local tasks share. A task starts when its share is free, in the
//! order the tasks asked.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use tokio::sync::{Mutex, Semaphore, SemaphorePermit};

const MIB: u64 = 1024 * 1024;

/// The CPUs and memory of this machine, or the part of them that tasks may use.
#[derive(Debug)]
pub struct Pool {
    cpus: Semaphore,
    /// In MiB.
    memory: Semaphore,
    total_cpus: u32,
    total_memory: u64,
    /// Held while a task takes its share, so that tasks take theirs one after another and
    /// none waits for memory while holding CPUs that an earlier task waits for.
    turn: Mutex<()>,
}

/// A task's share of the pool, given back when it is dropped.
#[derive(Debug)]
pub struct Share<'a> {
    _cpus: SemaphorePermit<'a>,
    _memory: Option<SemaphorePermit<'a>>,
    /// The CPUs and bytes of memory that the task may use: what it asked for, but at most all
    /// of the pool.
    pub cpus: u32,
    pub memory: Option<u64>,
}

impl Pool {
    /// A pool of `cpus` CPUs and `memory` bytes; both are at least 1 CPU and 1 MiB.
    pub fn new(cpus: u32, memory: u64) -> Pool {
        let total_cpus = cpus.max(1);
        let total_memory = memory.max(MIB);
        Pool {
            cpus: Semaphore::new(total_cpus as usize),
            memory: Semaphore::new(mebibytes(total_memory) as usize),
            total_cpus,
            total_memory,
            turn: Mutex::new(()),
        }
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

    /// Waits until a task's share is free. A task that asks for more than the pool has gets
    /// all of it; a task that names no memory takes none from the pool.
    pub async fn share(&self, cpus: u32, memory: Option<u64>) -> Share<'_> {
        let cpus = cpus.clamp(1, self.total_cpus);
        let memory = memory.map(|bytes| bytes.clamp(MIB, self.total_memory));
        let _turn = self.turn.lock().await;
        let cpu_permit = self.cpus.acquire_many(cpus).await;
        let memory_permit = match memory {
            Some(bytes) => Some(self.memory.acquire_many(mebibytes(bytes)).await),
            None => None,
        };
        // The semaphores are never closed, so a permit always comes.
        let (Ok(cpu_permit), Ok(memory_permit)) = (cpu_permit, memory_permit.transpose()) else {
            unreachable!("the pool's semaphores are never closed")
        };
        Share {
            _cpus: cpu_permit,
            _memory: memory_permit,
            cpus,
            memory,
        }
    }
}

fn mebibytes(bytes: u64) -> u32 {
    (bytes.div_ceil(MIB)).min(u32::MAX as u64 >> 3) as u32
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
    async fn tasks_wait_for_their_share() {
        let pool = Pool::new(4, 8 * 1024 * MIB);
        let first = pool.share(3, Some(2 * 1024 * MIB)).await;
        assert_eq!((first.cpus, first.memory), (3, Some(2 * 1024 * MIB)));
        // More than the pool has is all of it, once the first task is done.
        let waiting = pool.share(8, Some(100 * 1024 * MIB));
        let timeout = std::time::Duration::from_millis(50);
        let mut waiting = Box::pin(waiting);
        assert!(tokio::time::timeout(timeout, &mut waiting).await.is_err());
        drop(first);
        let all = waiting.await;
        assert_eq!((all.cpus, all.memory), (4, Some(8 * 1024 * MIB)));
    }
}
