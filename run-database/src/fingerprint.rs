//! The fingerprint of a task: what makes two tasks of different runs the same task with the same
//! inputs, so that a resumed run can take the result of a task that finished before.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use sha2::{Digest as _, Sha256};
use task_model::Digest;
use workflow_graph::{Resources, Task, TaskId, Writable};

/// The size and modification time of a local input when the task was asked for, as Nextflow's
/// standard cache mode compares them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputState {
    pub size: u64,
    /// Nanoseconds since the Unix epoch; for a folder, a digest of the names, sizes and times
    /// of all it holds.
    pub modified: u128,
}

/// The state of each input of a task, in the order of its inputs: `None` for literal content,
/// a URL, a shared folder, or a path that does not exist.
pub fn input_states(task: &Task) -> Vec<Option<InputState>> {
    task.inputs
        .iter()
        .map(|input| {
            // A shared folder counts by its path, as the runner keys it; its time changes
            // whenever a file in it comes or goes.
            if input.content.is_some()
                || input.source.contains("://")
                || input.writable == Writable::Shared
            {
                return None;
            }
            let metadata = std::fs::metadata(&input.source).ok()?;
            if metadata.is_dir() {
                return Some(folder_state(Path::new(&input.source)));
            }
            let modified = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
            Some(InputState {
                size: metadata.len(),
                modified: modified.as_nanos(),
            })
        })
        .collect()
}

/// The state of a folder from everything below it, in name order, so that a file changed in
/// place counts, which leaves the time of the folder itself as it was.
fn folder_state(folder: &Path) -> InputState {
    let mut hasher = Sha256::new();
    let mut size = 0;
    let mut pending = vec![folder.to_owned()];
    while let Some(current) = pending.pop() {
        let mut entries: Vec<PathBuf> = match std::fs::read_dir(&current) {
            Ok(entries) => entries.flatten().map(|entry| entry.path()).collect(),
            Err(error) => {
                hasher.update(format!("{}: {error}\n", current.display()));
                Vec::new()
            }
        };
        entries.sort();
        for path in entries {
            let relative = path.strip_prefix(folder).unwrap_or(&path);
            let Ok(found) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            let modified = (found.modified().ok())
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |time| time.as_nanos());
            let target = std::fs::read_link(&path).unwrap_or_default();
            hasher.update(format!(
                "{} {} {modified} {}\n",
                relative.display(),
                found.len(),
                target.display()
            ));
            match found.is_dir() {
                true => pending.push(path),
                false => size += found.len(),
            }
        }
    }
    let digest = hasher.finalize();
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&digest[..16]);
    InputState {
        size,
        modified: u128::from_be_bytes(bytes),
    }
}

/// The SHA-256 of the task as the driver described it, and of the state of each local input.
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
                ..Default::default()
            }],
            ..Task::default()
        }
    }

    #[test]
    fn fingerprints_ignore_ids() {
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

    /// A folder input counts by all it holds: a file changed in place, which leaves the time of
    /// the folder as it was, changes the fingerprint.
    #[test]
    fn folders_cover_contents() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let file = directory.path().join("a.txt");
        std::fs::write(&file, "one").expect("writes a file");
        let mut read = task(1, &directory.path().to_string_lossy());
        read.inputs[0].directory = true;
        let before = fingerprint(&read, &input_states(&read));
        assert_eq!(before, fingerprint(&read, &input_states(&read)));
        std::fs::write(&file, "two").expect("changes the file");
        let changed = std::fs::File::options()
            .write(true)
            .open(&file)
            .expect("opens");
        let time = UNIX_EPOCH + std::time::Duration::from_secs(1);
        changed.set_modified(time).expect("sets the time");
        assert_ne!(before, fingerprint(&read, &input_states(&read)));
    }

    /// A shared folder of Snakemake jobs keeps its fingerprint when files in it come and go.
    #[test]
    fn ignores_shared_folders() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let folder = directory.path().to_string_lossy().into_owned();
        let mut shared = task(1, &folder);
        shared.inputs[0].directory = true;
        shared.inputs[0].writable = Writable::Shared;
        let before = fingerprint(&shared, &input_states(&shared));
        std::fs::create_dir(directory.path().join("hello")).expect("makes a folder");
        assert_eq!(input_states(&shared), [None]);
        assert_eq!(before, fingerprint(&shared, &input_states(&shared)));
    }
}
