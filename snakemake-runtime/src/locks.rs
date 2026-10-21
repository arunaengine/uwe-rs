//! Locks the working directory against other runs with Snakemake's lock files.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

/// The lock files that the runs of this process hold, which go even when it exits at once.
static HELD: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Removes the lock files of the runs of this process, for an exit that drops no lock, like
/// a second Ctrl-C.
pub fn release_locks() {
    let held = std::mem::take(&mut *HELD.lock().unwrap_or_else(PoisonError::into_inner));
    remove(&held);
}

fn remove(files: &[PathBuf]) {
    for file in files {
        let _ = std::fs::remove_file(file);
    }
    // Only an empty folder goes: other runs may hold locks of their own.
    if let Some(folder) = files.first().and_then(|file| file.parent()) {
        let _ = std::fs::remove_dir(folder);
    }
}

/// The lock files that a run wrote in `.snakemake/locks`; they go when the lock is dropped.
#[derive(Debug)]
pub struct Lock {
    files: Vec<PathBuf>,
}

impl Lock {
    /// Locks the inputs and outputs of a run as Snakemake's `persistence.lock` does, or says
    /// why not: another run reads an output of this one, or writes a file this one uses.
    pub fn take(workdir: &Path, inputs: &[String], outputs: &[String]) -> Result<Lock, String> {
        let folder = workdir.join(".snakemake").join("locks");
        let failed = |error: std::io::Error| format!("cannot lock {}: {error}", folder.display());
        let mut stale = Vec::new();
        for (kind, file, lock) in held(&folder).map_err(failed)? {
            let taken = match kind {
                Kind::Input => outputs.contains(&file),
                Kind::Output => outputs.contains(&file) || inputs.contains(&file),
            };
            if taken && !stale.contains(&lock) {
                stale.push(lock);
            }
        }
        if !stale.is_empty() {
            return Err(refusal(workdir, &stale));
        }
        let mut lock = Lock { files: Vec::new() };
        for (kind, files) in [("input", inputs), ("output", outputs)] {
            if files.is_empty() {
                continue;
            }
            let path = write_lock(&folder, kind, files).map_err(failed)?;
            lock.files.push(path.clone());
            (HELD.lock().unwrap_or_else(PoisonError::into_inner)).push(path);
        }
        Ok(lock)
    }
}

/// Writes the first free lock file of a kind, `N.input.lock` or `N.output.lock`, with the
/// files it holds, and makes the folder again when another run removed it meanwhile.
fn write_lock(folder: &Path, kind: &str, files: &[String]) -> std::io::Result<PathBuf> {
    let mut number = 0;
    loop {
        let path = folder.join(format!("{number}.{kind}.lock"));
        let created = (std::fs::OpenOptions::new().write(true).create_new(true)).open(&path);
        match created {
            Ok(mut file) => {
                writeln!(file, "{}", files.join("\n"))?;
                return Ok(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => number += 1,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir_all(folder)?;
            }
            Err(error) => return Err(error),
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let mut held = HELD.lock().unwrap_or_else(PoisonError::into_inner);
        held.retain(|file| !self.files.contains(file));
        drop(held);
        remove(&self.files);
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Input,
    Output,
}

/// The files that the lock files of other runs name, as Snakemake's `_read_locks`, with the
/// lock file that names each.
fn held(folder: &Path) -> std::io::Result<Vec<(Kind, String, PathBuf)>> {
    let mut held = Vec::new();
    let entries = match std::fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(held),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some((number, kind)) = name.split_once('.') else {
            continue;
        };
        let kind = match kind {
            "input.lock" => Kind::Input,
            "output.lock" => Kind::Output,
            _ => continue,
        };
        if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        if entry.file_type()?.is_dir() {
            continue;
        }
        let text = std::fs::read_to_string(entry.path())?;
        for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
            held.push((kind, line.to_owned(), entry.path()));
        }
    }
    Ok(held)
}

/// Snakemake's `LockException`, which names the lock files in the way: iunctus has no
/// `--unlock`, and a stale lock goes when its files are removed.
fn refusal(workdir: &Path, stale: &[PathBuf]) -> String {
    let files: Vec<String> = (stale.iter())
        .map(|file| {
            file.strip_prefix(workdir)
                .unwrap_or(file)
                .display()
                .to_string()
        })
        .collect();
    format!(
        "LockException: Error: Directory cannot be locked. Please make sure that no other \
         Snakemake process is trying to create the same files in the following directory:\n{}\n\
         If you are sure that no other instances of snakemake are running on this directory, \
         the remaining lock was likely caused by a kill signal or a power loss. It can be \
         removed by removing these lock files: {}",
        workdir.display(),
        files.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_shared_files() {
        let folder = tempfile::tempdir().expect("a folder");
        let names = |files: &[&str]| {
            files
                .iter()
                .map(|file| file.to_string())
                .collect::<Vec<_>>()
        };
        let first = Lock::take(folder.path(), &names(&["a.txt"]), &names(&["b.txt"]));
        let first = first.expect("the first lock");
        let locks = folder.path().join(".snakemake/locks");
        let text = std::fs::read_to_string(locks.join("0.output.lock")).unwrap();
        assert_eq!(text, "b.txt\n");
        // Another run that writes what this one reads, or reads what it writes, waits.
        let refused = Lock::take(folder.path(), &[], &names(&["a.txt"])).unwrap_err();
        assert!(refused.contains("LockException"), "{refused}");
        assert!(
            refused.ends_with("lock files: .snakemake/locks/0.input.lock"),
            "{refused}"
        );
        let refused = Lock::take(folder.path(), &names(&["b.txt"]), &[]);
        assert!(refused.is_err());
        let other = Lock::take(folder.path(), &names(&["a.txt"]), &names(&["c.txt"]));
        let other = other.expect("a run of other files");
        assert!(locks.join("1.input.lock").is_file());
        drop(first);
        assert!(
            locks.join("1.output.lock").is_file(),
            "the other locks stay"
        );
        drop(other);
        assert!(!locks.exists());
        // The locks of this process go at an exit that drops none, like a second Ctrl-C.
        let kept = Lock::take(folder.path(), &[], &names(&["d.txt"])).expect("a lock");
        release_locks();
        assert!(!locks.exists());
        std::mem::forget(kept);
        // A folder that another run removed is made again.
        let path = write_lock(&locks, "input", &names(&["e.txt"])).expect("a lock file");
        assert!(path.is_file());
    }
}
