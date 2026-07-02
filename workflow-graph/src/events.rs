//! What happens in a run, as the engine reports it to an observer, like the run database.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::{Delivery, Edge, ExecuteError, Marker, Outcome, Task, TaskId};

/// One step of a run. The engine reports each one before its effect: a task before it is
/// queued, an attempt before the executor gets it, an outcome before the driver hears it.
#[derive(Debug)]
pub enum Event<'a> {
    /// The driver asked for a task; `edges` are the edges into it, from the tasks that made
    /// its input files and from those it runs after.
    Requested { task: &'a Task, edges: &'a [Edge] },
    /// An attempt of a task starts. Attempts count from 1.
    Started { task: TaskId, attempt: u32 },
    /// An attempt ended. When `retried` is set, the machine failed and the engine runs the
    /// task again; the driver does not hear of this attempt.
    Finished {
        outcome: &'a Outcome,
        attempt: u32,
        retried: bool,
    },
    /// A task got a marker: the tasks that follow it depend on its outputs, so a run with real
    /// data can have more or fewer tasks there. Once per new marker of a task.
    Marked { task: TaskId, marker: &'a Marker },
    /// A file that a task made goes to where the language expects its results. The event comes
    /// in simulated runs too, where nothing is delivered.
    Delivered { delivery: &'a Delivery },
    /// The run ended, with the error that stopped it if there is one.
    Ended { error: Option<&'a ExecuteError> },
}

/// Hears the events of a run, on the thread of the driver.
pub trait Observer {
    fn event(&mut self, event: Event<'_>);
}
