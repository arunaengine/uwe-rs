//! Embeds the source revision and the target triple, which `uwe --version` and run records print.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let folder = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("manifest folder"));
    println!("cargo:rerun-if-env-changed=UWE_SOURCE_REVISION");
    println!("cargo:rerun-if-changed=build.rs");
    let revision = git_revision(&folder)
        .or_else(|| std::env::var("UWE_SOURCE_REVISION").ok())
        .filter(|revision| !revision.trim().is_empty())
        .unwrap_or_else(|| "unknown".to_owned());
    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_owned());
    let version = std::env::var("CARGO_PKG_VERSION").expect("package version");
    println!("cargo:rustc-env=UWE_IDENTITY=uwe {version} ({revision} {target})");
}

/// The short commit hash, with `-dirty` after it when tracked files have changes. This is
/// `None` outside a git checkout of this workspace, as in a source tarball.
fn git_revision(folder: &Path) -> Option<String> {
    let root = PathBuf::from(git(folder, &["rev-parse", "--show-toplevel"])?);
    if root.canonicalize().ok()? != folder.parent()?.canonicalize().ok()? {
        return None;
    }
    watch_head(folder);
    let hash = git(folder, &["rev-parse", "--short", "HEAD"])?;
    let changes = git(folder, &["status", "--porcelain", "--untracked-files=no"])?;
    Some(if changes.is_empty() {
        hash
    } else {
        format!("{hash}-dirty")
    })
}

/// Asks cargo to build again when the checkout moves to another commit: `HEAD` changes on a
/// switch of branch, and the file of the current branch changes on a commit.
fn watch_head(folder: &Path) {
    let mut paths = vec!["HEAD".to_owned(), "packed-refs".to_owned()];
    if let Some(reference) = git(folder, &["symbolic-ref", "-q", "HEAD"]) {
        paths.push(reference);
    }
    for path in paths {
        if let Some(file) = git(folder, &["rev-parse", "--git-path", &path]) {
            let file = PathBuf::from(file);
            let file = if file.is_absolute() {
                file
            } else {
                folder.join(file)
            };
            println!("cargo:rerun-if-changed={}", file.display());
        }
    }
}

/// The trimmed output of a git command, or `None` when git is missing or the command fails.
fn git(folder: &Path, arguments: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(folder)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
