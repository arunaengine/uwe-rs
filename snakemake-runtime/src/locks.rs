//! Locks the working directory against other runs with Snakemake's lock files.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::Write;
use std::path::{Path, PathBuf};

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
        for (kind, file) in held(&folder).map_err(failed)? {
            let taken = match kind {
                Kind::Input => outputs.contains(&file),
                Kind::Output => outputs.contains(&file) || inputs.contains(&file),
            };
            if taken {
                return Err(refusal(workdir));
            }
        }
        std::fs::create_dir_all(&folder).map_err(failed)?;
        let mut lock = Lock { files: Vec::new() };
        for (kind, files) in [("input", inputs), ("output", outputs)] {
            if files.is_empty() {
                continue;
            }
            for number in 0.. {
                let path = folder.join(format!("{number}.{kind}.lock"));
                let created = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path);
                let mut file = match created {
                    Ok(file) => file,
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => return Err(failed(error)),
                };
                lock.files.push(path);
                writeln!(file, "{}", files.join("\n")).map_err(failed)?;
                break;
            }
        }
        Ok(lock)
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        for file in &self.files {
            let _ = std::fs::remove_file(file);
        }
        // Only an empty folder goes: other runs may hold locks of their own.
        if let Some(folder) = self.files.first().and_then(|file| file.parent()) {
            let _ = std::fs::remove_dir(folder);
        }
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Input,
    Output,
}

/// The files that the lock files of other runs name, as Snakemake's `_read_locks`.
fn held(folder: &Path) -> std::io::Result<Vec<(Kind, String)>> {
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
            held.push((kind, line.to_owned()));
        }
    }
    Ok(held)
}

/// Snakemake's `LockException`; iunctus has no `--unlock`, so it names the folder to remove.
fn refusal(workdir: &Path) -> String {
    format!(
        "LockException: Error: Directory cannot be locked. Please make sure that no other \
         Snakemake process is trying to create the same files in the following directory:\n{}\n\
         If you are sure that no other instances of snakemake are running on this directory, \
         the remaining lock was likely caused by a kill signal or a power loss. It can be \
         removed with `snakemake --unlock`, or by removing the folder .snakemake/locks.",
        workdir.display()
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
        let refused = Lock::take(folder.path(), &[], &names(&["a.txt"]));
        assert!(refused.unwrap_err().contains("LockException"));
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
    }
}
