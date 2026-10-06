//! Combines workflow disk files, finished task outputs, and buffered planning writes.
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
    /// Whether the writes of workflow code also reach the disk, as in a real run, where
    /// Snakemake's Python writes files while it plans. A simulated run keeps them in memory.
    on_disk: std::cell::Cell<bool>,
    pub lifecycle: std::cell::Cell<Option<&'static str>>,
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
            on_disk: std::cell::Cell::new(false),
            lifecycle: std::cell::Cell::new(None),
        }
    }

    /// Lets the writes of workflow code reach the disk, for a real run.
    pub fn write_to_disk(&self) {
        self.on_disk.set(true);
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

    pub fn local_location(&self, location: &Path) -> std::io::Result<PathBuf> {
        match self.task_files.borrow().get() {
            Some(files) => files.local(&location.to_string_lossy()),
            None => Ok(location.to_path_buf()),
        }
    }

    /// Where a path is on disk: in the output of a task, or itself. A path inside a directory
    /// output of a task that the folder does not hold is not in that output.
    pub fn location(&self, path: &Path) -> Option<(PathBuf, bool)> {
        let produced = self.produced.borrow();
        for ancestor in path.ancestors() {
            if let Some(entry) = produced.get(ancestor) {
                if ancestor == path {
                    return Some((entry.location.clone(), entry.directory));
                }
                if entry.directory {
                    let rest = path.strip_prefix(ancestor).unwrap_or(path);
                    let inner = entry.location.join(rest);
                    // A folder on this machine tells whether the path is in it, so that a file
                    // that another job makes there is not taken as made. A folder of a store
                    // elsewhere cannot tell.
                    if let Ok(local) = self.local_location(&entry.location)
                        && local.is_dir()
                    {
                        let metadata = std::fs::metadata(local.join(rest)).ok()?;
                        return Some((inner, metadata.is_dir()));
                    }
                    return Some((inner, false));
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
            let metadata = self
                .local_location(&location)
                .ok()
                .and_then(|local| std::fs::metadata(local).ok());
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
            for entry in std::fs::read_dir(self.local_location(&location)?)? {
                let entry = entry?;
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
        self.local.write(path, data, append)?;
        if self.on_disk.get() {
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .append(append)
                .truncate(!append)
                .open(path)?;
            std::io::Write::write_all(&mut file, data)?;
        }
        Ok(())
    }

    fn make_dir(&self, path: &Path) -> std::io::Result<()> {
        self.local.make_dir(path)?;
        if self.on_disk.get() {
            std::fs::create_dir(path)?;
        }
        Ok(())
    }

    fn remove(&self, path: &Path) -> std::io::Result<()> {
        // What the disk holds before the code removes it in memory.
        let on_disk = (self.on_disk.get())
            .then(|| std::fs::symlink_metadata(path).ok())
            .flatten();
        self.local.remove(path)?;
        match on_disk {
            Some(metadata) if metadata.is_dir() => std::fs::remove_dir(path),
            Some(_) => std::fs::remove_file(path),
            None => Ok(()),
        }
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
            Some(runner) if self.lifecycle.get().is_some() => {
                runner.run_hook(command, self.lifecycle.get().unwrap())
            }
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
    fn reads_stored_outputs() {
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

    #[test]
    fn persists_run_writes() {
        let folder = tempfile::tempdir().expect("a temporary directory");
        let storage = storage::Storage::default();
        let mirrors = Rc::new(Mirrors::new(folder.path().join("downloads"), &storage));
        let files = WorkflowFiles::new(folder.path(), mirrors);
        let path = folder.path().join("a.txt");
        files.write(&path, b"in memory\n", false).expect("writes");
        assert!(!path.exists());
        files.write_to_disk();
        files.write(&path, b"a\n", false).expect("writes");
        files.write(&path, b"b\n", true).expect("appends");
        files
            .make_dir(&folder.path().join("logs"))
            .expect("makes a folder");
        assert_eq!(std::fs::read_to_string(&path).expect("reads"), "a\nb\n");
        assert!(folder.path().join("logs").is_dir());
        files.remove(&path).expect("removes");
        assert!(!path.exists());
    }

    #[test]
    fn lists_stored_directories() {
        let folder = tempfile::tempdir().expect("a temporary directory");
        std::fs::create_dir_all(folder.path().join("checkpoint/nested")).expect("makes folders");
        std::fs::write(folder.path().join("checkpoint/counts.txt"), "3\n").expect("writes");
        std::fs::write(folder.path().join("checkpoint/nested/a.txt"), "a\n").expect("writes");
        let storage = storage::Storage::default();
        let mirrors = Rc::new(Mirrors::new(folder.path().join("downloads"), &storage));
        let files = WorkflowFiles::new(&folder.path().join("workflow"), mirrors);
        let path = folder.path().join("workflow/results");
        files.add_output(
            &path,
            Produced {
                location: PathBuf::from("s3://store/checkpoint"),
                directory: true,
            },
        );
        let slot: Arc<OnceLock<Arc<dyn Files>>> = Arc::default();
        files.read_outputs_through(slot.clone());
        let _ = slot.set(Arc::new(Elsewhere(folder.path().to_owned())));
        assert_eq!(files.list(&path).expect("lists"), ["counts.txt", "nested"]);
        assert_eq!(files.list(&path.join("nested")).expect("lists"), ["a.txt"]);
        assert_eq!(
            files.read(&path.join("nested/a.txt")).expect("reads"),
            b"a\n"
        );
        assert_eq!(
            files.stat(&path.join("nested")).map(|stat| stat.kind),
            Some(FileKind::Directory)
        );
        assert!(files.location(&path.join("missing.txt")).is_none());
    }

    #[test]
    fn preserves_missing_files() {
        let folder = tempfile::tempdir().expect("a temporary directory");
        let made = folder.path().join("made");
        std::fs::create_dir(&made).expect("makes the folder");
        std::fs::write(made.join("f0.fasta"), ">a\n").expect("writes");
        let storage = storage::Storage::default();
        let mirrors = Rc::new(Mirrors::new(folder.path().join("downloads"), &storage));
        let files = WorkflowFiles::new(&folder.path().join("work"), mirrors);
        let output = folder.path().join("work/files");
        let produced = Produced {
            location: made.clone(),
            directory: true,
        };
        files.add_output(&output, produced);
        let fasta = files.location(&output.join("f0.fasta"));
        assert_eq!(fasta, Some((made.join("f0.fasta"), false)));
        // Another job makes `files/f0.txt` later, as in Snakemake's test_github_issue988.
        assert_eq!(files.location(&output.join("f0.txt")), None);
        assert!(files.stat(&output.join("f0.txt")).is_none());
    }
}
