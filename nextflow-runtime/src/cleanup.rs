//! Frees the files of a run's own folder once no node of the run can pass them on, for nf-gc and
//! nf-boost's `boost.cleanup`. Task results stay: other runs, the cache and publications share them.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use nextflow_evaluator::value::Value;

/// The files of the run's folder that the network carried, like those of `collectFile`, the
/// splitters and `exec:` sections, with the nodes that sent each.
pub(crate) struct Cleanup {
    folder: PathBuf,
    files: BTreeMap<String, BTreeSet<usize>>,
    /// Files that a delivery takes from the folder, which stay until the run ends.
    kept: HashSet<String>,
}

impl Cleanup {
    pub(crate) fn new(folder: PathBuf) -> Cleanup {
        Cleanup {
            folder,
            files: BTreeMap::new(),
            kept: HashSet::new(),
        }
    }

    /// Notes the files of the folder in a value that `node` sent.
    pub(crate) fn sent(&mut self, node: usize, value: &Value) {
        match value {
            Value::Path(path) if path.prefix.is_none() => {
                let inside = Path::new(&path.path);
                if inside.starts_with(&self.folder) && inside != self.folder {
                    self.files
                        .entry(path.path.clone())
                        .or_default()
                        .insert(node);
                }
            }
            Value::List(list) => {
                for item in &list.borrow().items {
                    self.sent(node, item);
                }
            }
            Value::Map(map) => {
                for (key, item) in &map.borrow().entries {
                    self.sent(node, &key.0);
                    self.sent(node, item);
                }
            }
            Value::Set(set) => {
                for item in &set.borrow().items {
                    self.sent(node, &item.0);
                }
            }
            Value::MapEntry(entry) => {
                self.sent(node, &entry.0);
                self.sent(node, &entry.1);
            }
            _ => {}
        }
    }

    /// Keeps a file that a delivery takes, like a published one.
    pub(crate) fn delivered(&mut self, location: &str) {
        if Path::new(location).starts_with(&self.folder) {
            self.kept.insert(location.to_owned());
        }
    }

    /// Deletes each file whose senders are all `done`, as one tree with the files inside it or
    /// around it; a directory goes whole, without following links.
    pub(crate) fn sweep(&mut self, mut done: impl FnMut(usize) -> bool) {
        let mut answers = HashMap::new();
        let mut ready = |node: usize| *answers.entry(node).or_insert_with(|| done(node));
        let free: HashSet<String> = (self.files.iter())
            .filter(|(path, nodes)| {
                !self.kept.contains(*path) && nodes.iter().all(|node| ready(*node))
            })
            .map(|(path, _)| path.clone())
            .collect();
        let related = |one: &str, other: &str| {
            Path::new(one).starts_with(other) || Path::new(other).starts_with(one)
        };
        let blocked: Vec<&String> = (self.files.keys().chain(&self.kept))
            .filter(|path| !free.contains(*path))
            .collect();
        let mut gone: Vec<String> = (free.iter())
            .filter(|path| !blocked.iter().any(|other| related(path, other)))
            .cloned()
            .collect();
        gone.sort();
        let mut removed: Vec<&str> = Vec::new();
        for path in &gone {
            self.files.remove(path);
            if removed
                .iter()
                .any(|parent| Path::new(path).starts_with(parent))
            {
                continue;
            }
            match delete(Path::new(path)) {
                Ok(()) => tracing::debug!(path, "the cleanup deleted a file of the run"),
                Err(error) => tracing::warn!(path, %error, "the cleanup could not delete a file"),
            }
            removed.push(path);
        }
    }
}

fn delete(path: &Path) -> std::io::Result<()> {
    let result = std::fs::symlink_metadata(path).and_then(|found| match found.is_dir() {
        true => std::fs::remove_dir_all(path),
        false => std::fs::remove_file(path),
    });
    match result {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}
