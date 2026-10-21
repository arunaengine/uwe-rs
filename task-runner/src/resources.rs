//! Right-sized resources for the tasks of a run, from what the tasks of the same step used in
//! earlier runs (see [`task_model::right_sized`]).
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use task_model::{Measured, Placement, right_sized};
use workflow_graph::Task;

/// The bytes of the input files of a task, counted as the run database counts them: literal
/// content, and local files that are not directories. `None` when no input has a known size.
pub fn input_bytes(task: &Task) -> Option<u64> {
    (task.inputs.iter())
        .filter_map(|input| match &input.content {
            Some(content) => Some(content.len() as u64),
            None if input.directory || input.source.contains("://") => None,
            None => std::fs::metadata(&input.source).ok().map(|file| file.len()),
        })
        .reduce(u64::saturating_add)
}

/// The placement of the first attempt of a task, right-sized by what the earlier tasks of its
/// step used. A retry keeps what the task asks for, since the first attempt may have failed
/// for lack of memory.
pub fn placement(task: &Task, asked: &Placement, attempt: u32, measured: &[Measured]) -> Placement {
    match attempt <= 1 && !measured.is_empty() {
        true => right_sized(asked, input_bytes(task), measured),
        false => asked.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use workflow_graph::Input;

    #[test]
    fn retries_preserve_requests() {
        let task = Task {
            inputs: vec![Input {
                content: Some("abc".into()),
                ..Input::default()
            }],
            ..Task::default()
        };
        assert_eq!(input_bytes(&task), Some(3));
        let asked = Placement {
            cpus: 8,
            memory_bytes: Some(1 << 34),
            ..Placement::default()
        };
        let measured = [Measured {
            memory_peak: Some(1 << 20),
            cpu_usec: Some(1_000_000),
            milliseconds: 1000,
            input_bytes: Some(3),
            overhead: None,
        }];
        let first = placement(&task, &asked, 1, &measured);
        assert_eq!(first.cpus, 2);
        assert_eq!(first.memory_bytes, Some(task_model::LEAST_MEMORY));
        assert_eq!(placement(&task, &asked, 2, &measured), asked);
        assert_eq!(placement(&task, &asked, 1, &[]), asked);
    }
}
