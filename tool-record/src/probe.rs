//! Runs a command once in its folder and finds the files and folders it wrote.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0
// Moved from SciWIn (https://github.com/fairagro/sciwin), Copyright (c) 2025 FAIRagro,
// MIT OR Apache-2.0; see LICENSE-SCIWIN.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;

use crate::Error;
use crate::command::OPERATORS;

/// What a folder holds: every path below it, relative to it, with its stamp.
pub(crate) type Snapshot = BTreeMap<PathBuf, Stamp>;

/// Enough of a file to see that a command wrote it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Stamp {
    pub folder: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
}

/// Every path below the folder. Names that start with `.`, like `.git`, are left out.
pub(crate) fn snapshot(folder: &Path) -> Result<Snapshot, Error> {
    let mut found = Snapshot::new();
    let mut pending = vec![PathBuf::new()];
    while let Some(relative) = pending.pop() {
        let path = folder.join(&relative);
        let read = |source| Error::Read {
            path: path.clone(),
            source,
        };
        for entry in fs::read_dir(&path).map_err(read)? {
            let entry = entry.map_err(read)?;
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let metadata = entry.metadata().map_err(read)?;
            let name = relative.join(entry.file_name());
            if metadata.is_dir() {
                pending.push(name.clone());
            }
            let stamp = Stamp {
                folder: metadata.is_dir(),
                size: metadata.len(),
                modified: metadata.modified().ok(),
            };
            found.insert(name, stamp);
        }
    }
    Ok(found)
}

/// Runs the command in the folder, through `sh -c` when it has shell operators, and gives
/// back what it printed. A command that fails is an error.
pub(crate) fn run(words: &[String], folder: &Path) -> Result<String, Error> {
    let (program, arguments) = words.split_first().ok_or(Error::Empty)?;
    let mut command = match words.iter().any(|word| OPERATORS.contains(&word.as_str())) {
        true => {
            let mut shell = Command::new("sh");
            shell.arg("-c").arg(line(words));
            shell
        }
        false => {
            let mut plain = Command::new(program);
            plain.args(arguments);
            plain
        }
    };
    let output = command
        .current_dir(folder)
        .stdin(Stdio::null())
        .output()
        .map_err(|source| Error::Start {
            program: program.clone(),
            source,
        })?;
    let log = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    match output.status.success() {
        true => Ok(log),
        false => Err(Error::Failed {
            status: output.status.to_string(),
            log,
        }),
    }
}

/// The paths that are new or changed after the run. A new folder stands for everything in it.
pub(crate) fn changed(before: &Snapshot, after: &Snapshot) -> Vec<PathBuf> {
    let new_folder = |path: &Path| {
        (after.get(path)).is_some_and(|stamp| stamp.folder) && !before.contains_key(path)
    };
    let inside = |path: &Path| path.ancestors().skip(1).any(new_folder);
    (after.iter())
        .filter(|(path, stamp)| match stamp.folder {
            true => new_folder(path),
            false => before.get(*path) != Some(stamp),
        })
        .map(|(path, _)| path.clone())
        .filter(|path| !inside(path))
        .collect()
}

/// The command as one shell line: operators stay as they are, every other word is quoted.
fn line(words: &[String]) -> String {
    let quoted = words
        .iter()
        .map(|word| match OPERATORS.contains(&word.as_str()) {
            true => word.clone(),
            false => format!("'{}'", word.replace('\'', r"'\''")),
        });
    quoted.collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_plain_words() {
        let words = ["echo", "it's", ">", "out.txt"].map(String::from);
        assert_eq!(line(&words), r"'echo' 'it'\''s' > 'out.txt'");
    }

    #[test]
    fn folders_hide_contents() {
        let stamp = |folder| Stamp {
            folder,
            size: 1,
            modified: None,
        };
        let before = Snapshot::from([("kept.txt".into(), stamp(false))]);
        let mut after = before.clone();
        after.insert("results".into(), stamp(true));
        after.insert("results/a.txt".into(), stamp(false));
        after.insert("new.txt".into(), stamp(false));
        let found = changed(&before, &after);
        assert_eq!(found, [PathBuf::from("new.txt"), PathBuf::from("results")]);
    }
}
