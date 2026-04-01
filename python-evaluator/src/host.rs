//! What the interpreter asks of its host: files, the working directory, output and the
//! environment. Evaluation never writes to disk: [`LocalHost`] reads real files and keeps
//! everything that code writes in memory.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    File,
    Directory,
}

#[derive(Clone, Copy, Debug)]
pub struct Stat {
    pub kind: FileKind,
    pub size: u64,
    /// Seconds since 1970.
    pub modified: f64,
}

pub trait Host {
    /// The working directory, absolute.
    fn cwd(&self) -> PathBuf;

    fn set_cwd(&self, path: &Path);

    fn read(&self, path: &Path) -> std::io::Result<Vec<u8>>;

    fn stat(&self, path: &Path) -> Option<Stat>;

    /// The names in a directory, sorted.
    fn list(&self, path: &Path) -> std::io::Result<Vec<String>>;

    fn write(&self, path: &Path, data: &[u8], append: bool) -> std::io::Result<()>;

    fn make_dir(&self, path: &Path) -> std::io::Result<()>;

    fn remove(&self, path: &Path) -> std::io::Result<()>;

    /// Output of `print` and of writes to `sys.stdout` and `sys.stderr`.
    fn print(&self, text: &str);

    fn environment(&self) -> Vec<(String, String)>;

    /// An absolute, normalized path for a path relative to the working directory.
    fn resolve(&self, path: &str) -> PathBuf {
        let path = expand_home(path);
        let joined = if Path::new(&path).is_absolute() {
            PathBuf::from(path)
        } else {
            self.cwd().join(path)
        };
        normalize(&joined)
    }
}

/// `~` at the start of a path as the home directory.
pub fn expand_home(path: &str) -> String {
    if (path == "~" || path.starts_with("~/"))
        && let Some(home) = std::env::var_os("HOME")
    {
        return format!("{}{}", home.to_string_lossy(), &path[1..]);
    }
    path.to_owned()
}

/// A path without `.` and `..` components, as `os.path.normpath` of an absolute path.
pub fn normalize(path: &Path) -> PathBuf {
    let mut normal = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normal.pop();
            }
            other => normal.push(other),
        }
    }
    normal
}

/// Reads the real file system and keeps writes, new directories and removals in memory.
pub struct LocalHost {
    cwd: RefCell<PathBuf>,
    written: RefCell<BTreeMap<PathBuf, Vec<u8>>>,
    directories: RefCell<BTreeSet<PathBuf>>,
    removed: RefCell<BTreeSet<PathBuf>>,
    pub output: RefCell<String>,
    /// Prints output to standard error as well as keeping it.
    pub echo: bool,
}

impl LocalHost {
    pub fn new(cwd: impl Into<PathBuf>) -> LocalHost {
        LocalHost {
            cwd: RefCell::new(cwd.into()),
            written: RefCell::new(BTreeMap::new()),
            directories: RefCell::new(BTreeSet::new()),
            removed: RefCell::new(BTreeSet::new()),
            output: RefCell::new(String::new()),
            echo: false,
        }
    }

    fn is_removed(&self, path: &Path) -> bool {
        let removed = self.removed.borrow();
        path.ancestors().any(|ancestor| removed.contains(ancestor))
    }
}

impl Host for LocalHost {
    fn cwd(&self) -> PathBuf {
        self.cwd.borrow().clone()
    }

    fn set_cwd(&self, path: &Path) {
        *self.cwd.borrow_mut() = path.to_owned();
    }

    fn read(&self, path: &Path) -> std::io::Result<Vec<u8>> {
        if let Some(data) = self.written.borrow().get(path) {
            return Ok(data.clone());
        }
        if self.is_removed(path) {
            return Err(std::io::ErrorKind::NotFound.into());
        }
        std::fs::read(path)
    }

    fn stat(&self, path: &Path) -> Option<Stat> {
        if let Some(data) = self.written.borrow().get(path) {
            return Some(Stat {
                kind: FileKind::File,
                size: data.len() as u64,
                modified: 0.0,
            });
        }
        if self.directories.borrow().contains(path) {
            return Some(Stat {
                kind: FileKind::Directory,
                size: 0,
                modified: 0.0,
            });
        }
        if self.is_removed(path) {
            return None;
        }
        let metadata = std::fs::metadata(path).ok()?;
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0.0, |duration| duration.as_secs_f64());
        Some(Stat {
            kind: if metadata.is_dir() {
                FileKind::Directory
            } else {
                FileKind::File
            },
            size: metadata.len(),
            modified,
        })
    }

    fn list(&self, path: &Path) -> std::io::Result<Vec<String>> {
        let mut names: BTreeSet<String> = BTreeSet::new();
        let mut found = false;
        if !self.is_removed(path)
            && let Ok(entries) = std::fs::read_dir(path)
        {
            found = true;
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if !self.is_removed(&path.join(&name)) {
                    names.insert(name);
                }
            }
        }
        let written = self.written.borrow();
        let directories = self.directories.borrow();
        for other in written.keys().chain(directories.iter()) {
            if other == path {
                found = true;
            }
            if let Ok(rest) = other.strip_prefix(path)
                && let Some(Component::Normal(first)) = rest.components().next()
            {
                found = true;
                names.insert(first.to_string_lossy().into_owned());
            }
        }
        if !found {
            return Err(std::io::ErrorKind::NotFound.into());
        }
        Ok(names.into_iter().collect())
    }

    fn write(&self, path: &Path, data: &[u8], append: bool) -> std::io::Result<()> {
        let mut written = self.written.borrow_mut();
        let entry = written.entry(path.to_owned()).or_default();
        if !append {
            entry.clear();
        }
        entry.extend_from_slice(data);
        self.removed.borrow_mut().remove(path);
        Ok(())
    }

    fn make_dir(&self, path: &Path) -> std::io::Result<()> {
        self.directories.borrow_mut().insert(path.to_owned());
        self.removed.borrow_mut().remove(path);
        Ok(())
    }

    fn remove(&self, path: &Path) -> std::io::Result<()> {
        self.written.borrow_mut().remove(path);
        self.directories.borrow_mut().remove(path);
        self.removed.borrow_mut().insert(path.to_owned());
        Ok(())
    }

    fn print(&self, text: &str) {
        if self.echo {
            eprint!("{text}");
        }
        self.output.borrow_mut().push_str(text);
    }

    fn environment(&self) -> Vec<(String, String)> {
        std::env::vars().collect()
    }
}
