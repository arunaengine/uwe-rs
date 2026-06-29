//! The fingerprint of a task: what makes two tasks of different runs the same task with the same
//! inputs, so that a resumed run can take the result of a task that finished before.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::time::UNIX_EPOCH;

use sha2::{Digest as _, Sha256};
use task_model::Digest;
use workflow_graph::{Resources, Task, TaskId};

/// The size and modification time of a local input when the task was asked for, as Nextflow's
/// standard cache mode compares them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputState {
    pub size: u64,
    /// Nanoseconds since the Unix epoch.
    pub modified: u128,
}

/// The state of each input of a task, in the order of its inputs: `None` for literal content,
/// a URL, or a path that does not exist.
pub fn input_states(task: &Task) -> Vec<Option<InputState>> {
    task.inputs
        .iter()
        .map(|input| {
            if input.content.is_some() || input.source.contains("://") {
                return None;
            }
            let metadata = std::fs::metadata(&input.source).ok()?;
            let modified = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
            Some(InputState {
                size: metadata.len(),
                modified: modified.as_nanos(),
            })
        })
        .collect()
}

/// The SHA-256 of the task as the driver described it, and of the state of each local input.
/// What differs between runs of the same task is left out: its id and `after` list, its name
/// for people, which can hold a number in the order tasks finished, like Nextflow's
/// `COUNT (2)`, and its resources, as Nextflow leaves them out of its hash. Outputs of earlier
/// tasks are inputs by their location, so a task after a task that ran again has a new
/// fingerprint.
pub fn fingerprint(task: &Task, states: &[Option<InputState>]) -> String {
    let described = Task {
        id: TaskId(0),
        name: String::new(),
        resources: Resources::default(),
        after: Vec::new(),
        ..task.clone()
    };
    let mut hasher = Sha256::new();
    // Serializing a task cannot fail: its fields are strings, numbers, lists and maps.
    hasher.update(serde_json::to_vec(&described).unwrap_or_default());
    for state in states {
        match state {
            Some(state) => hasher.update(format!("\n{} {}", state.size, state.modified)),
            None => hasher.update("\n-"),
        }
    }
    Digest::from_sha256(&hasher.finalize()).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use workflow_graph::Input;

    fn task(id: u64, source: &str) -> Task {
        Task {
            id: TaskId(id),
            name: "P (1)".into(),
            step: "P".into(),
            command: vec!["bash".into(), ".command.sh".into()],
            inputs: vec![Input {
                path: "in.txt".into(),
                source: source.into(),
                directory: false,
                content: None,
            }],
            ..Task::default()
        }
    }

    #[test]
    fn ids_do_not_change_the_fingerprint_but_inputs_do() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let file = directory.path().join("in.txt");
        std::fs::write(&file, "one").expect("writes the input");
        let source = file.to_string_lossy();
        let first = task(1, &source);
        let mut second = task(7, &source);
        second.after = vec![TaskId(3)];
        second.name = "P (7)".into();
        second.resources.cpus = Some(4.0);
        let states = input_states(&first);
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].map(|state| state.size), Some(3));
        assert_eq!(
            fingerprint(&first, &states),
            fingerprint(&second, &input_states(&second))
        );
        std::fs::write(&file, "three").expect("changes the input");
        assert_ne!(
            fingerprint(&first, &states),
            fingerprint(&second, &input_states(&second))
        );
        let elsewhere = task(1, "/no/such/file");
        assert_eq!(input_states(&elsewhere), [None]);
        assert_ne!(
            fingerprint(&first, &states),
            fingerprint(&elsewhere, &input_states(&elsewhere))
        );
    }
}
