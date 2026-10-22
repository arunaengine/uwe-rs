//! Locks the working directory against other runs with Snakemake's lock files.
// Copyright (c) 2026 The uwe Contributors
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
        let folder = file.parent().expect("a lock folder");
        let workdir = folder.parent().and_then(Path::parent).expect("a workdir");
        let _guard = guard(workdir).ok();
        let _ = std::fs::remove_file(file);
        let _ = std::fs::remove_dir(folder);
    }
}

fn guard(workdir: &Path) -> std::io::Result<std::fs::File> {
    let folder = workdir.join(".snakemake");
    std::fs::create_dir_all(&folder)?;
    // Keep this inode outside the removable lock directory so every process shares it.
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(folder.join("uwe-planning.lock"))?;
    rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)?;
    Ok(file)
}

/// The lock files that a run wrote in `.snakemake/locks`; they go when the lock is dropped.
#[derive(Debug)]
pub struct Lock {
    files: Vec<PathBuf>,
    workdir: PathBuf,
}

impl Lock {
    /// Locks the inputs and outputs of a run as Snakemake's `persistence.lock` does, or says
    /// why not: another run reads an output of this one, or writes a file this one uses.
    pub fn take(workdir: &Path, inputs: &[String], outputs: &[String]) -> Result<Lock, String> {
        let mut lock = Lock {
            files: Vec::new(),
            workdir: workdir.to_owned(),
        };
        lock.extend(inputs, outputs)?;
        Ok(lock)
    }

    pub fn extend(&mut self, inputs: &[String], outputs: &[String]) -> Result<(), String> {
        let folder = self.workdir.join(".snakemake").join("locks");
        let failed = |error: std::io::Error| format!("cannot lock {}: {error}", folder.display());
        let _guard = guard(&self.workdir).map_err(failed)?;
        let mut inputs = inputs.to_vec();
        let mut outputs = outputs.to_vec();
        let mut stale = Vec::new();
        for (kind, file, lock) in held(&folder).map_err(failed)? {
            if self.files.contains(&lock) {
                match kind {
                    Kind::Input => inputs.retain(|input| input != &file),
                    Kind::Output => outputs.retain(|output| output != &file),
                }
                continue;
            }
            let taken = match kind {
                Kind::Input => outputs.contains(&file),
                Kind::Output => outputs.contains(&file) || inputs.contains(&file),
            };
            if taken && !stale.contains(&lock) {
                stale.push(lock);
            }
        }
        if !stale.is_empty() {
            return Err(refusal(&self.workdir, &stale));
        }
        for (kind, files) in [("input", &inputs), ("output", &outputs)] {
            if files.is_empty() {
                continue;
            }
            let path = write_lock(&folder, kind, files).map_err(failed)?;
            self.files.push(path.clone());
            (HELD.lock().unwrap_or_else(PoisonError::into_inner)).push(path);
        }
        Ok(())
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
                if let Err(error) = writeln!(file, "{}", files.join("\n")) {
                    let _ = std::fs::remove_file(&path);
                    return Err(error);
                }
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

/// Snakemake's `LockException`, which names the lock files in the way: uwe has no
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
    use std::io::BufRead;

    static SERIAL: Mutex<()> = Mutex::new(());

    #[test]
    fn extends_conflict_locks() {
        let _serial = SERIAL.lock().unwrap();
        let folder = tempfile::tempdir().unwrap();
        let mut first = Lock::take(folder.path(), &["input".into()], &["seed".into()]).unwrap();
        first
            .extend(&["input".into()], &["seed".into(), "dynamic".into()])
            .unwrap();
        assert!(Lock::take(folder.path(), &[], &["dynamic".into()]).is_err());
        let other = Lock::take(folder.path(), &[], &["occupied".into()]).unwrap();
        assert!(first.extend(&[], &["occupied".into()]).is_err());
        assert!(Lock::take(folder.path(), &[], &["seed".into()]).is_err());
        drop(other);
        first.extend(&[], &["occupied".into()]).unwrap();
    }

    #[test]
    fn process_child() {
        let Some(folder) = std::env::var_os("UWE_LOCK_TEST") else {
            return;
        };
        let mut input = std::io::stdin().lock();
        let mut line = String::new();
        println!("\nUWE_READY");
        std::io::stdout().flush().unwrap();
        input.read_line(&mut line).unwrap();
        let lock = Lock::take(Path::new(&folder), &[], &["output".into()]);
        println!("\nUWE_RESULT:{}", lock.is_ok());
        std::io::stdout().flush().unwrap();
        line.clear();
        input.read_line(&mut line).unwrap();
        drop(lock);
    }

    #[test]
    fn serializes_processes() {
        let folder = tempfile::tempdir().unwrap();
        let mut children = Vec::new();
        for _ in 0..2 {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "locks::tests::process_child", "--nocapture"])
                .env("UWE_LOCK_TEST", folder.path())
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let mut output = std::io::BufReader::new(child.stdout.take().unwrap());
            assert_eq!(read_signal(&mut output, "UWE_READY"), "UWE_READY");
            children.push((child, output));
        }
        for (child, _) in &mut children {
            writeln!(child.stdin.as_mut().unwrap(), "start").unwrap();
        }
        let accepted: usize = children
            .iter_mut()
            .map(|(_, output)| usize::from(read_signal(output, "UWE_RESULT:") == "UWE_RESULT:true"))
            .sum();
        for (child, _) in &mut children {
            writeln!(child.stdin.as_mut().unwrap(), "release").unwrap();
            assert!(child.wait().unwrap().success());
        }
        assert_eq!(accepted, 1, "only one process may own the output");
    }

    fn read_signal(input: &mut impl BufRead, prefix: &str) -> String {
        loop {
            let mut line = String::new();
            assert_ne!(
                input.read_line(&mut line).unwrap(),
                0,
                "child exited before {prefix}"
            );
            if let Some((_, signal)) = line.split_once(prefix) {
                return format!("{prefix}{}", signal.trim());
            }
        }
    }

    #[test]
    fn refuses_shared_files() {
        let _serial = SERIAL.lock().unwrap();
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
