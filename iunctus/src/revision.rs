//! The Git commit of a workflow, read from the `.git` folder of its repository without a Git
//! program, so that each run records which version of the workflow it ran.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fs::read_to_string;
use std::path::{Path, PathBuf};

/// The commit that `HEAD` names in the repository that holds `path`, when it is in one. Works
/// for plain repositories, worktrees and submodules, whose `.git` is a file that names the
/// real Git folder, and for references packed into `packed-refs`.
pub fn revision(path: &Path) -> Option<String> {
    let (_, git) = repository(path)?;
    let head = read_to_string(git.join("HEAD")).ok()?;
    let head = head.trim();
    let Some(reference) = head.strip_prefix("ref:") else {
        return commit(head);
    };
    let reference = reference.trim();
    // A worktree keeps its branches in the common folder of the repository.
    let common: PathBuf = match read_to_string(git.join("commondir")) {
        Ok(text) => git.join(text.trim()),
        Err(_) => git.clone(),
    };
    for folder in [&git, &common] {
        if let Some(found) = read_to_string(folder.join(reference))
            .ok()
            .and_then(|text| commit(text.trim()))
        {
            return Some(found);
        }
    }
    let packed = read_to_string(common.join("packed-refs")).ok()?;
    packed.lines().find_map(|line| {
        let (hash, name) = line.split_once(' ')?;
        if name == reference {
            commit(hash)
        } else {
            None
        }
    })
}

/// The folder of the work tree and the Git folder of the repository that holds `path`, when it
/// is in one. A work tree of `git worktree` and a submodule have a `.git` file that names their
/// Git folder.
pub fn repository(path: &Path) -> Option<(PathBuf, PathBuf)> {
    let start = std::path::absolute(path).ok()?;
    let (root, dot_git) = start.ancestors().find_map(|folder| {
        let dot_git = folder.join(".git");
        dot_git.exists().then(|| (folder.to_owned(), dot_git))
    })?;
    if dot_git.is_dir() {
        return Some((root, dot_git));
    }
    let text = read_to_string(&dot_git).ok()?;
    let target = text.strip_prefix("gitdir:")?.trim();
    Some((root.clone(), root.join(target)))
}

/// The text when it is a SHA-1 or SHA-256 commit id.
fn commit(text: &str) -> Option<String> {
    let hex = text.bytes().all(|byte| byte.is_ascii_hexdigit());
    (hex && (text.len() == 40 || text.len() == 64)).then(|| text.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    const OTHER: &str = "89abcdef0123456789abcdef0123456789abcdef";

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("makes folders");
        std::fs::write(path, text).expect("writes");
    }

    #[test]
    fn branches_packed_references_and_detached_heads() {
        let root = tempfile::tempdir().expect("a temporary directory");
        let repository = root.path().join("repository");
        let workflow = repository.join("workflows/main.nf");
        write(&workflow, "");
        write(&repository.join(".git/HEAD"), "ref: refs/heads/main\n");
        write(
            &repository.join(".git/refs/heads/main"),
            &format!("{COMMIT}\n"),
        );
        assert_eq!(revision(&workflow).as_deref(), Some(COMMIT));
        std::fs::remove_file(repository.join(".git/refs/heads/main")).expect("removes");
        let packed = format!("# pack-refs with: peeled\n{OTHER} refs/heads/main\n");
        write(&repository.join(".git/packed-refs"), &packed);
        assert_eq!(revision(&workflow).as_deref(), Some(OTHER));
        write(&repository.join(".git/HEAD"), &format!("{COMMIT}\n"));
        assert_eq!(revision(&workflow).as_deref(), Some(COMMIT));
        assert_eq!(revision(&root.path().join("elsewhere")), None);
    }

    #[test]
    fn worktrees_read_their_branch_from_the_common_folder() {
        let root = tempfile::tempdir().expect("a temporary directory");
        let common = root.path().join("main/.git");
        write(&common.join("refs/heads/feature"), &format!("{OTHER}\n"));
        let git = common.join("worktrees/feature");
        write(&git.join("HEAD"), "ref: refs/heads/feature\n");
        write(&git.join("commondir"), "../..\n");
        let worktree = root.path().join("feature");
        write(
            &worktree.join(".git"),
            &format!("gitdir: {}\n", git.display()),
        );
        assert_eq!(revision(&worktree).as_deref(), Some(OTHER));
    }
}
