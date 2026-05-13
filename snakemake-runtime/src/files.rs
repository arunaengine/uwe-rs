//! The files a workflow sees while it is planned: its working directory on disk, the outputs
//! of finished tasks, which live where the executor put them, and what code writes, which
//! stays in memory.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

use python_evaluator::{Command, CommandOutcome, Error, FileKind, Host, LocalHost, Stat};

use crate::commands::CommandRunner;

/// An output of a finished task: where it is now, and whether it is a directory.
#[derive(Clone, Debug)]
pub struct Produced {
    pub location: PathBuf,
    pub directory: bool,
}

pub struct WorkflowFiles {
    local: LocalHost,
    /// Outputs of finished tasks by their absolute path in the working directory.
    produced: RefCell<BTreeMap<PathBuf, Produced>>,
    /// Output of `print` and of logging while the workflow is planned.
    pub messages: RefCell<Vec<String>>,
    /// Runs the commands of workflow code as tasks, once the driver connects it.
    commands: RefCell<Option<Rc<CommandRunner>>>,
}

impl WorkflowFiles {
    pub fn new(workdir: &Path) -> WorkflowFiles {
        WorkflowFiles {
            local: LocalHost::new(workdir),
            produced: RefCell::new(BTreeMap::new()),
            messages: RefCell::new(Vec::new()),
            commands: RefCell::new(None),
        }
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
            Some((location, _)) => std::fs::read(location),
            None => self.local.read(path),
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
