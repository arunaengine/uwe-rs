//! The files a workflow sees while it is planned: its working directory on disk, the outputs
//! of finished tasks, which live where the executor put them, and what code writes, which
//! stays in memory.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use python_evaluator::{Command, CommandOutcome, Error, FileKind, Host, LocalHost, Stat};
use workflow_graph::{Files, Note};

use crate::commands::CommandRunner;
use crate::remote::Mirrors;

/// An output of a finished task: where it is now, and whether it is a directory.
#[derive(Clone, Debug)]
pub struct Produced {
    pub location: PathBuf,
    pub directory: bool,
}

pub struct WorkflowFiles {
    local: LocalHost,
    /// Local copies of workflow files on the web, downloaded when they are read.
    mirrors: Rc<Mirrors>,
    /// Outputs of finished tasks by their absolute path in the working directory.
    produced: RefCell<BTreeMap<PathBuf, Produced>>,
    /// Output of `print` and of logging while the workflow is planned.
    pub messages: RefCell<Vec<String>>,
    /// Runs the commands of workflow code as tasks, once the driver connects it.
    commands: RefCell<Option<Rc<CommandRunner>>>,
    /// The last output of a task that workflow code read.
    pub last_output_read: RefCell<Option<PathBuf>>,
    /// The outputs of tasks that workflow code read or listed since the engine last asked,
    /// by their location, for the markers of the graph.
    pub notes: RefCell<Vec<Note>>,
    /// The files of the tasks, where a real executor keeps them, which the driver gets from
    /// the engine; outputs are read through them.
    task_files: RefCell<Arc<OnceLock<Arc<dyn Files>>>>,
}

impl WorkflowFiles {
    pub fn new(workdir: &Path, mirrors: Rc<Mirrors>) -> WorkflowFiles {
        WorkflowFiles {
            local: LocalHost::new(workdir),
            mirrors,
            produced: RefCell::new(BTreeMap::new()),
            messages: RefCell::new(Vec::new()),
            commands: RefCell::new(None),
            last_output_read: RefCell::new(None),
            notes: RefCell::new(Vec::new()),
            task_files: RefCell::new(Arc::default()),
        }
    }

    /// Reads the outputs of tasks through the files that the driver gets from the engine.
    pub fn read_outputs_through(&self, files: Arc<OnceLock<Arc<dyn Files>>>) {
        *self.task_files.borrow_mut() = files;
    }

    /// Connects the runner of commands of workflow code.
    pub fn set_commands(&self, runner: Rc<CommandRunner>) {
        *self.commands.borrow_mut() = Some(runner);
    }

    /// Records an output of a finished task.
    pub fn add_output(&self, path: &Path, produced: Produced) {
        self.produced.borrow_mut().insert(path.to_owned(), produced);
    }

    /// Where a path is on disk: in the output of a task, or itself.
    pub fn location(&self, path: &Path) -> Option<(PathBuf, bool)> {
        let produced = self.produced.borrow();
        for ancestor in path.ancestors() {
            if let Some(entry) = produced.get(ancestor) {
                if ancestor == path {
                    return Some((entry.location.clone(), entry.directory));
                }
                if entry.directory {
                    let rest = path.strip_prefix(ancestor).unwrap_or(path);
                    return Some((entry.location.join(rest), false));
                }
            }
        }
        None
    }
}

impl Host for WorkflowFiles {
    fn cwd(&self) -> PathBuf {
        self.local.cwd()
    }

    fn set_cwd(&self, path: &Path) {
        self.local.set_cwd(path);
    }

    fn read(&self, path: &Path) -> std::io::Result<Vec<u8>> {
        match self.location(path) {
            Some((location, _)) => {
                *self.last_output_read.borrow_mut() = Some(path.to_owned());
                self.notes.borrow_mut().push(Note::Read(location.clone()));
                let files = self.task_files.borrow().get().cloned();
                workflow_graph::files::read(files.as_deref(), &location.to_string_lossy())
            }
            None => {
                self.mirrors.fetch(path)?;
                self.local.read(path)
            }
        }
    }

    fn stat(&self, path: &Path) -> Option<Stat> {
        if let Some((location, directory)) = self.location(path) {
            let metadata = std::fs::metadata(&location).ok();
            return Some(Stat {
                kind: if directory || metadata.as_ref().is_some_and(|metadata| metadata.is_dir()) {
                    FileKind::Directory
                } else {
                    FileKind::File
                },
                size: metadata.map_or(0, |metadata| metadata.len()),
                modified: 0.0,
            });
        }
        // A file on the web that cannot be downloaded counts as missing.
        let _ = self.mirrors.fetch(path);
        if let Some(stat) = self.local.stat(path) {
            return Some(stat);
        }
        // A folder that only holds outputs of tasks.
        let produced = self.produced.borrow();
        produced
            .keys()
            .any(|output| output.starts_with(path) && output != path)
            .then_some(Stat {
                kind: FileKind::Directory,
                size: 0,
                modified: 0.0,
            })
    }

    fn list(&self, path: &Path) -> std::io::Result<Vec<String>> {
        let mut names: BTreeSet<String> = BTreeSet::new();
        let mut found = false;
        if let Some((location, _)) = self.location(path) {
            self.notes.borrow_mut().push(Note::Read(location.clone()));
            for entry in std::fs::read_dir(location)?.flatten() {
                names.insert(entry.file_name().to_string_lossy().into_owned());
            }
            found = true;
        }
        if let Ok(local) = self.local.list(path) {
            names.extend(local);
            found = true;
        }
        for output in self.produced.borrow().keys() {
            if let Ok(rest) = output.strip_prefix(path)
                && let Some(Component::Normal(first)) = rest.components().next()
            {
                names.insert(first.to_string_lossy().into_owned());
                found = true;
            }
        }
        if !found {
            return Err(std::io::ErrorKind::NotFound.into());
        }
        Ok(names.into_iter().collect())
    }

    fn write(&self, path: &Path, data: &[u8], append: bool) -> std::io::Result<()> {
        self.local.write(path, data, append)
    }

    fn make_dir(&self, path: &Path) -> std::io::Result<()> {
        self.local.make_dir(path)
    }

    fn remove(&self, path: &Path) -> std::io::Result<()> {
        self.local.remove(path)
    }

    fn print(&self, text: &str) {
        let mut messages = self.messages.borrow_mut();
        messages.extend(text.lines().map(str::to_owned));
    }

    fn environment(&self) -> Vec<(String, String)> {
        self.local.environment()
    }

    fn run_command(&self, command: &Command) -> Result<CommandOutcome, Error> {
        let runner = self.commands.borrow().clone();
        match runner {
            Some(runner) => runner.run(command),
            None => Err(Error::unsupported(
                "running commands while the workflow is planned",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A store elsewhere, whose `s3://store/` locations have their content in a local folder.
    struct Elsewhere(PathBuf);

    impl Files for Elsewhere {
        fn local(&self, location: &str) -> std::io::Result<PathBuf> {
            let key = location.strip_prefix("s3://store/");
            Ok(self.0.join(key.ok_or(std::io::ErrorKind::NotFound)?))
        }

        fn deliver(&self, _delivery: &workflow_graph::Delivery) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn outputs_of_a_store_elsewhere_are_read_through_the_files_of_the_run() {
        let folder = tempfile::tempdir().expect("a temporary directory");
        std::fs::write(folder.path().join("counts.txt"), "3\n").expect("writes");
        let storage = storage::Storage::default();
        let mirrors = Rc::new(Mirrors::new(folder.path().join("downloads"), &storage));
        let files = WorkflowFiles::new(folder.path(), mirrors);
        let path = folder.path().join("results/counts.txt");
        let produced = Produced {
            location: PathBuf::from("s3://store/counts.txt"),
            directory: false,
        };
        files.add_output(&path, produced);
        let slot: Arc<OnceLock<Arc<dyn Files>>> = Arc::default();
        files.read_outputs_through(slot.clone());
        assert!(files.read(&path).is_err(), "no local file is there");
        let _ = slot.set(Arc::new(Elsewhere(folder.path().to_owned())));
        assert_eq!(files.read(&path).expect("reads"), b"3\n");
    }
}
