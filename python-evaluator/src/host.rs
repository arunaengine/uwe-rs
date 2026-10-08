//! Provides interpreter files, output, environment, and working directory; writes stay in memory.
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

/// A command that code asks to run, like with `subprocess.run` or `os.system`.
#[derive(Clone, Debug)]
pub struct Command {
    /// The program and its arguments; a shell command is `/bin/sh -c <text>`.
    pub argv: Vec<String>,
    /// The folder the command runs in, absolute.
    pub cwd: PathBuf,
}

/// What a command did: its exit code and its output.
#[derive(Clone, Debug, Default)]
pub struct CommandOutcome {
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

pub trait Host {
    /// The working directory, absolute.
    fn cwd(&self) -> PathBuf;

    fn set_cwd(&self, path: &Path);

    fn read(&self, path: &Path) -> std::io::Result<Vec<u8>>;

    fn stat(&self, path: &Path) -> Option<Stat>;

    /// Associates a workflow path with provider metadata for planning.
    fn map_storage(&self, _: &Path, _: &str) -> std::io::Result<()> {
        Ok(())
    }

    /// Whether a path is a symbolic link, without following it.
    fn is_symlink(&self, path: &Path) -> bool {
        std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink())
    }

    /// A stable identity for cycle checks while following directory links.
    fn canonicalize(&self, path: &Path) -> PathBuf {
        std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned())
    }

    /// The names in a directory, sorted.
    fn list(&self, path: &Path) -> std::io::Result<Vec<String>>;

    fn write(&self, path: &Path, data: &[u8], append: bool) -> std::io::Result<()>;

    fn make_dir(&self, path: &Path) -> std::io::Result<()>;

    fn remove(&self, path: &Path) -> std::io::Result<()>;

    /// Output of `print` and of writes to `sys.stdout` and `sys.stderr`.
    fn print(&self, text: &str);

    fn environment(&self) -> Vec<(String, String)>;

    /// Runs a command that code asks for. Commands never run on the machine of iunctus
    /// itself (decision 11); a host that can run them in a container overrides this.
    fn run_command(&self, command: &Command) -> Result<CommandOutcome, crate::error::Error> {
        let _ = command;
        Err(crate::error::Error::unsupported(
            "running commands while the workflow is planned",
        ))
    }

    /// Whether the process may use a path as `os.access` asks: `mode` combines 4 (read),
    /// 2 (write) and 1 (execute), and 0 only asks whether it exists.
    fn access(&self, path: &Path, mode: i64) -> bool {
        self.stat(path).is_some() && (mode == 0 || permitted(path, mode))
    }

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

/// The user and group ids of the process, from `/proc/self/status`.
fn process_ids() -> Option<(u32, Vec<u32>)> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let field = |name: &str| -> Vec<u32> {
        status
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .map(|rest| {
                rest.split_whitespace()
                    .filter_map(|id| id.parse().ok())
                    .collect()
            })
            .unwrap_or_default()
    };
    let user = *field("Uid:").get(1)?;
    let mut groups = field("Groups:");
    groups.extend(field("Gid:").get(1));
    Some((user, groups))
}

/// Whether the permission bits of a file on disk give the process the access of `mode`. A
/// path that is not on disk, like one that code wrote, is accessible.
#[cfg(unix)]
fn permitted(path: &Path, mode: i64) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(metadata) = std::fs::metadata(path) else {
        return true;
    };
    let Some((user, groups)) = process_ids() else {
        return true;
    };
    let bits = metadata.mode();
    if user == 0 {
        return mode & 1 == 0 || bits & 0o111 != 0 || metadata.is_dir();
    }
    let shift = if metadata.uid() == user {
        6
    } else if groups.contains(&metadata.gid()) {
        3
    } else {
        0
    };
    let allowed = i64::from((bits >> shift) & 0o7);
    mode & !allowed & 0o7 == 0
}

#[cfg(not(unix))]
fn permitted(_: &Path, _: i64) -> bool {
    true
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

    fn is_symlink(&self, path: &Path) -> bool {
        if self.written.borrow().contains_key(path)
            || self.directories.borrow().contains(path)
            || self.is_removed(path)
        {
            return false;
        }
        std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink())
    }

    fn canonicalize(&self, path: &Path) -> PathBuf {
        if self.written.borrow().contains_key(path)
            || self.directories.borrow().contains(path)
            || self.is_removed(path)
        {
            return path.to_owned();
        }
        std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned())
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
        let initial = if append && !self.written.borrow().contains_key(path) {
            match self.read(path) {
                Ok(data) => data,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
                Err(error) => return Err(error),
            }
        } else {
            Vec::new()
        };
        let mut written = self.written.borrow_mut();
        let entry = written.entry(path.to_owned()).or_insert(initial);
        if !append {
            entry.clear();
        }
        entry.extend_from_slice(data);
        self.removed.borrow_mut().remove(path);
        Ok(())
    }

    fn make_dir(&self, path: &Path) -> std::io::Result<()> {
        // As on the system, a new folder needs the permission to write into its parent.
        if let Some(parent) = path.parent()
            && !path.exists()
            && !permitted(parent, 3)
        {
            return Err(std::io::ErrorKind::PermissionDenied.into());
        }
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn requires_parent_permission() {
        let folder = tempfile::tempdir().expect("a folder");
        let locked = folder.path().join("locked");
        std::fs::create_dir(&locked).expect("a locked folder");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555))
            .expect("read-only permissions");
        let host = LocalHost::new(folder.path());
        let denied = host.make_dir(&locked.join("new"));
        let made = host.make_dir(&folder.path().join("free"));
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755))
            .expect("the permissions back");
        assert!(made.is_ok());
        // The superuser may write anywhere.
        if process_ids().is_some_and(|(user, _)| user == 0) {
            return;
        }
        assert_eq!(
            denied.map_err(|error| error.kind()),
            Err(std::io::ErrorKind::PermissionDenied)
        );
    }
}
