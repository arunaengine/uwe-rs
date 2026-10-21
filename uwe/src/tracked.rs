//! Reads tracked file paths from Git index versions 2, 3 and 4.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

/// The size of the fixed part of an entry before its object name: times, device, inode, mode,
/// user, group and size, ten numbers of four bytes.
const STATS: usize = 40;

/// The mode of a submodule, whose entry names a commit, not a file.
const GITLINK: u32 = 0o160000;

/// The paths of the files that the index of a Git folder lists, relative to the work tree, each
/// once and sorted, without submodules. `None` when the index cannot be read.
pub fn tracked(git: &Path) -> Option<Vec<String>> {
    let bytes = std::fs::read(git.join("index")).ok()?;
    let hash = object_name_size(git);
    parse(&bytes, hash)
}

/// The size of object names: 32 bytes in a repository of SHA-256 objects, else 20 for SHA-1.
fn object_name_size(git: &Path) -> usize {
    let config = std::fs::read_to_string(git.join("config")).unwrap_or_default();
    let sha256 = config.lines().any(|line| {
        let line = line.trim().to_ascii_lowercase().replace(' ', "");
        line == "objectformat=sha256"
    });
    if sha256 { 32 } else { 20 }
}

/// The paths of an index: a header of `DIRC`, the version and the number of entries, then the
/// entries, sorted by path.
fn parse(bytes: &[u8], hash: usize) -> Option<Vec<String>> {
    if bytes.get(..4)? != b"DIRC" {
        return None;
    }
    let version = number(bytes, 4)?;
    if !(2..=4).contains(&version) {
        return None;
    }
    let count = number(bytes, 8)? as usize;
    let mut at = 12;
    let mut paths: Vec<String> = Vec::with_capacity(count);
    let mut previous: Vec<u8> = Vec::new();
    for _ in 0..count {
        let start = at;
        let mode = number(bytes, at + 24)?;
        at += STATS + hash;
        let flags = u16::from_be_bytes(bytes.get(at..at + 2)?.try_into().ok()?);
        at += 2;
        // Version 3 has more flags when the extended flag is set.
        if version >= 3 && flags & 0x4000 != 0 {
            at += 2;
        }
        let path = if version == 4 {
            // How many bytes of the previous path to drop, then the rest of this one.
            let (drop, length) = varint(bytes.get(at..)?)?;
            at += length;
            let end = at + bytes.get(at..)?.iter().position(|byte| *byte == 0)?;
            let mut path = previous.get(..previous.len().checked_sub(drop)?)?.to_vec();
            path.extend_from_slice(&bytes[at..end]);
            at = end + 1;
            path
        } else {
            let end = at + bytes.get(at..)?.iter().position(|byte| *byte == 0)?;
            let path = bytes[at..end].to_vec();
            // Entries are padded with NUL bytes to a multiple of eight.
            let length = end - start;
            at = start + (length + 8) / 8 * 8;
            path
        };
        let stage = (flags >> 12) & 0x3;
        if mode != GITLINK && stage <= 1 {
            paths.push(String::from_utf8(path.clone()).ok()?);
        }
        previous = path;
    }
    paths.dedup();
    Some(paths)
}

/// A big-endian number of four bytes.
fn number(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// A number of Git's offset encoding, and the bytes it takes: seven bits per byte, the highest
/// bit set on all bytes but the last, and one added for each byte after the first.
fn varint(bytes: &[u8]) -> Option<(usize, usize)> {
    let mut value = usize::from(*bytes.first()? & 0x7f);
    let mut used = 1;
    while bytes.get(used - 1)? & 0x80 != 0 {
        let next = *bytes.get(used)?;
        value = value.checked_add(1)?.checked_mul(128)? + usize::from(next & 0x7f);
        used += 1;
    }
    Some((value, used))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Git program makes the index; the code under test reads it without one.
    fn git(folder: &Path, arguments: &[&str]) {
        let status = std::process::Command::new("git")
            .args(arguments)
            .current_dir(folder)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .expect("runs git");
        assert!(status.success(), "git {arguments:?}");
    }

    #[test]
    fn lists_tracked_files() {
        let root = tempfile::tempdir().expect("a temporary directory");
        let folder = root.path();
        git(folder, &["init", "-q"]);
        let files = [
            "main.nf",
            "modules/local/count/main.nf",
            "modules/local/count/meta.yml",
            "nextflow.config",
            "a-very-long-name-of-a-folder/with-a-long-file-name-that-pads.txt",
        ];
        for file in files {
            let path = folder.join(file);
            std::fs::create_dir_all(path.parent().expect("a parent")).expect("makes folders");
            std::fs::write(&path, file).expect("writes");
        }
        std::fs::write(folder.join("untracked.txt"), "x").expect("writes");
        git(
            folder,
            &["add", "--", "main.nf", "modules", "nextflow.config"],
        );
        git(folder, &["add", "--", "a-very-long-name-of-a-folder"]);
        let mut expected: Vec<String> = files.iter().map(|file| (*file).to_owned()).collect();
        expected.sort();
        let found = crate::revision::repository(&folder.join("modules/local"));
        let (work, dot_git) = found.expect("a repository");
        assert_eq!(work, folder);
        for version in ["2", "3", "4"] {
            git(folder, &["update-index", "--index-version", version]);
            assert_eq!(
                tracked(&dot_git),
                Some(expected.clone()),
                "version {version}"
            );
        }
        assert_eq!(crate::revision::repository(Path::new("/")), None);
    }

    #[test]
    fn decodes_git_offsets() {
        assert_eq!(varint(&[0x05]), Some((5, 1)));
        assert_eq!(varint(&[0x80, 0x00]), Some((128, 2)));
        assert_eq!(varint(&[0x81, 0x01]), Some((257, 2)));
        assert_eq!(varint(&[0x80]), None);
        assert_eq!(parse(b"nope", 20), None);
    }
}
