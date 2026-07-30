//! The files that git tracks below a folder, as `git ls-files --recurse-submodules .` lists
//! them, read from the index of the repository without git (decision 26).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

/// The files and links that git tracks below `folder`, as absolute paths, also those of the
/// submodules there; `None` when the folder is in no repository or its index cannot be read.
/// Entries that the working tree does not have, as in a sparse checkout, are left out.
pub fn tracked(folder: &Path) -> Option<Vec<PathBuf>> {
    let folder = std::fs::canonicalize(folder).ok()?;
    let (top, git) = repository(&folder)?;
    let mut files = Vec::new();
    listed(&top, &git, &mut files).ok()?;
    files.retain(|file| file.starts_with(&folder));
    files.sort();
    files.dedup();
    Some(files)
}

/// The top folder of the repository that holds `folder`, and its git folder.
fn repository(folder: &Path) -> Option<(PathBuf, PathBuf)> {
    let mut current = Some(folder);
    while let Some(candidate) = current {
        if let Some(git) = git_folder(candidate) {
            return Some((candidate.to_owned(), git));
        }
        current = candidate.parent();
    }
    None
}

/// The git folder of a working tree: its `.git` folder, or the folder that a `.git` file names
/// with `gitdir:`, as worktrees and submodules have.
fn git_folder(top: &Path) -> Option<PathBuf> {
    let dot = top.join(".git");
    if dot.is_dir() {
        return Some(dot);
    }
    let text = std::fs::read_to_string(&dot).ok()?;
    let named = text.trim().strip_prefix("gitdir:")?.trim();
    Some(top.join(named))
}

/// Adds the files that the index of the working tree `top` lists, and those of its
/// submodules.
fn listed(top: &Path, git: &Path, files: &mut Vec<PathBuf>) -> Result<(), IndexError> {
    let bytes = std::fs::read(git.join("index")).map_err(|_| IndexError)?;
    for entry in entries(&bytes, hash_length(git))? {
        let path = top.join(&entry.path);
        match entry.mode >> 12 {
            // Regular files and symbolic links.
            0o10 | 0o12 => {
                if std::fs::symlink_metadata(&path).is_ok() {
                    files.push(path);
                }
            }
            // A submodule: its own index lists its files.
            0o16 => {
                if let Some(inner) = git_folder(&path) {
                    let _ = listed(&path, &inner, files);
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// The length of object names in the repository: 32 bytes for SHA-256, else 20 for SHA-1.
fn hash_length(git: &Path) -> usize {
    // A worktree keeps its config in the common folder that `commondir` names.
    let common = match std::fs::read_to_string(git.join("commondir")) {
        Ok(text) => git.join(text.trim()),
        Err(_) => git.to_owned(),
    };
    let config = std::fs::read_to_string(common.join("config")).unwrap_or_default();
    let sha256 = config.lines().any(|line| {
        let line = line.trim().to_ascii_lowercase().replace(' ', "");
        line == "objectformat=sha256"
    });
    if sha256 { 32 } else { 20 }
}

/// The index of a repository could not be read.
#[derive(Debug)]
struct IndexError;

/// An entry of the index: its mode and its path relative to the top folder.
#[derive(Debug, PartialEq, Eq)]
struct Entry {
    mode: u32,
    path: String,
}

/// The entries of an index of version 2, 3 or 4, as `Documentation/gitformat-index.txt`
/// describes them.
fn entries(bytes: &[u8], hash: usize) -> Result<Vec<Entry>, IndexError> {
    let mut reader = Reader { bytes, at: 0 };
    if reader.take(4)? != b"DIRC" {
        return Err(IndexError);
    }
    let version = reader.number()?;
    if !(2..=4).contains(&version) {
        return Err(IndexError);
    }
    let count = reader.number()?;
    let mut entries = Vec::new();
    let mut previous: Vec<u8> = Vec::new();
    for _ in 0..count {
        let start = reader.at;
        // ctime, mtime, dev and ino come before the mode; uid, gid and size after it.
        reader.take(24)?;
        let mode = reader.number()?;
        reader.take(12 + hash)?;
        let flags = u16::from_be_bytes(reader.take(2)?.try_into().map_err(|_| IndexError)?);
        if version >= 3 && flags & 0x4000 != 0 {
            reader.take(2)?;
        }
        let path = if version == 4 {
            let removed = reader.varint()?;
            let kept = previous.len().checked_sub(removed).ok_or(IndexError)?;
            let mut path = previous[..kept].to_vec();
            path.extend_from_slice(reader.until_nul()?);
            path
        } else {
            let path = reader.until_nul()?.to_vec();
            // The entry is padded with NUL bytes to a multiple of eight bytes.
            let length = reader.at - start;
            reader.take((8 - length % 8) % 8)?;
            path
        };
        let text = String::from_utf8(path.clone()).map_err(|_| IndexError)?;
        entries.push(Entry { mode, path: text });
        previous = path;
    }
    Ok(entries)
}

/// Reads the index from the front.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, length: usize) -> Result<&'a [u8], IndexError> {
        let end = self.at.checked_add(length).ok_or(IndexError)?;
        let taken = self.bytes.get(self.at..end).ok_or(IndexError)?;
        self.at = end;
        Ok(taken)
    }

    /// A 32-bit number in network order.
    fn number(&mut self) -> Result<u32, IndexError> {
        let bytes = self.take(4)?;
        Ok(u32::from_be_bytes(
            bytes.try_into().map_err(|_| IndexError)?,
        ))
    }

    /// The bytes up to the next NUL byte, which is taken too.
    fn until_nul(&mut self) -> Result<&'a [u8], IndexError> {
        let rest = self.bytes.get(self.at..).ok_or(IndexError)?;
        let length = rest.iter().position(|byte| *byte == 0).ok_or(IndexError)?;
        let taken = &rest[..length];
        self.at += length + 1;
        Ok(taken)
    }

    /// A number in git's variable-length encoding of offsets: seven bits per byte, the high
    /// bit set on all but the last byte, and one added before each shift.
    fn varint(&mut self) -> Result<usize, IndexError> {
        let mut byte = self.take(1)?[0];
        let mut value = usize::from(byte & 0x7f);
        while byte & 0x80 != 0 {
            byte = self.take(1)?[0];
            value = value
                .checked_add(1)
                .and_then(|value| value.checked_mul(128))
                .ok_or(IndexError)?
                + usize::from(byte & 0x7f);
        }
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs git in a folder, for the repositories of the tests.
    fn git(folder: &Path, arguments: &[&str]) {
        let status = std::process::Command::new("git")
            .args(arguments)
            .current_dir(folder)
            .output()
            .expect("runs git");
        assert!(status.status.success(), "{status:?}");
    }

    fn write(folder: &Path, path: &str) {
        let file = folder.join(path);
        std::fs::create_dir_all(file.parent().expect("a parent")).expect("makes the folder");
        std::fs::write(file, path).expect("writes");
    }

    fn relative(files: &[PathBuf], folder: &Path) -> Vec<String> {
        let folder = std::fs::canonicalize(folder).expect("a path");
        (files.iter())
            .map(|file| {
                file.strip_prefix(&folder)
                    .expect("inside")
                    .display()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn tracked_files_come_from_the_index_of_every_version() {
        let root = tempfile::tempdir().expect("a temporary directory");
        let top = root.path();
        git(top, &["init", "-q"]);
        for path in [
            "Snakefile",
            "scripts/a.py",
            "scripts/deeper/b.py",
            "sub/some_package/__init__.py",
            "untracked.txt",
        ] {
            write(top, path);
        }
        std::os::unix::fs::symlink("a.py", top.join("scripts/link.py")).expect("links");
        git(top, &["add", "Snakefile", "scripts", "sub"]);
        for version in ["2", "3", "4"] {
            git(top, &["update-index", "--index-version", version]);
            let files = tracked(top).expect("a repository");
            let expected = [
                "Snakefile",
                "scripts/a.py",
                "scripts/deeper/b.py",
                "scripts/link.py",
                "sub/some_package/__init__.py",
            ];
            assert_eq!(relative(&files, top), expected, "version {version}");
        }
        // Below a folder, only its files count, as `git ls-files .` there lists them.
        let files = tracked(&top.join("scripts")).expect("a repository");
        let expected = ["a.py", "deeper/b.py", "link.py"];
        assert_eq!(relative(&files, &top.join("scripts")), expected);
    }

    #[test]
    fn repositories_with_sha256_names_are_read_too() {
        let root = tempfile::tempdir().expect("a temporary directory");
        let top = root.path();
        git(top, &["init", "-q", "--object-format=sha256"]);
        write(top, "scripts/a.py");
        git(top, &["add", "scripts"]);
        let files = tracked(top).expect("a repository");
        assert_eq!(relative(&files, top), ["scripts/a.py"]);
    }

    #[test]
    fn submodules_add_their_files_and_folders_outside_git_have_none() {
        let root = tempfile::tempdir().expect("a temporary directory");
        let top = root.path();
        git(top, &["init", "-q"]);
        write(top, "Snakefile");
        git(top, &["add", "Snakefile"]);
        // A submodule is a repository of its own, which the index of the top one names by a
        // commit, without its files.
        let inner = top.join("vendor");
        std::fs::create_dir(&inner).expect("makes the folder");
        git(&inner, &["init", "-q"]);
        write(&inner, "lib/tool.py");
        git(&inner, &["add", "."]);
        let gitlink = format!("160000,{},vendor", "1".repeat(40));
        git(top, &["update-index", "--add", "--cacheinfo", &gitlink]);
        let files = tracked(top).expect("a repository");
        assert_eq!(relative(&files, top), ["Snakefile", "vendor/lib/tool.py"]);
        assert_eq!(tracked(Path::new("/")), None);
    }
}
