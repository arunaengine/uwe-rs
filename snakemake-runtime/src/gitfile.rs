//! The git repositories of `gitfile(...)`, written out at their reference: one of this machine
//! read with gix, one on the web fetched over HTTPS by the Git client of the run.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gix::bstr::ByteSlice;
use gix::objs::tree::EntryKind;
use sha2::Digest;

/// Writes the files of the repository at an address, at a branch, tag or commit, into a
/// folder that does not exist yet.
pub type Checkout = dyn Fn(&str, &str, &Path) -> Result<(), String> + Send + Sync;

/// The Git client of a run, for repositories on the web.
#[derive(Clone)]
pub struct GitClient(pub Arc<Checkout>);

impl std::fmt::Debug for GitClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("GitClient")
    }
}

/// Whether a repository of `gitfile(...)` is on the web rather than on this machine.
pub fn on_web(repository: &str) -> bool {
    repository.starts_with("https://") || repository.starts_with("http://")
}

/// The folder with the files of a repository of this machine at a reference, below
/// `downloads`. Its name is the commit, so a branch that moves gives a new folder.
pub fn local(repository: &Path, reference: &str, downloads: &Path) -> Result<PathBuf, String> {
    let text = |error: &dyn std::fmt::Display| error.to_string();
    let options = gix::open::Options::isolated();
    let found = gix::open_opts(repository, options).map_err(|error| text(&error))?;
    let commit = commit_of(&found, reference)?;
    let folder = downloads.join("git").join(commit.to_hex().to_string());
    if folder.is_dir() {
        return Ok(folder);
    }
    let tree = (found.find_commit(commit))
        .and_then(|commit| commit.tree_id())
        .map_err(|error| text(&error))?;
    written(&folder, |target| write_tree(&found, tree.detach(), target))?;
    Ok(folder)
}

/// The folder with the files of a repository on the web at a reference, fetched again with
/// `fresh`, as a run reads the reference as it is now.
pub fn web(
    client: &GitClient,
    url: &str,
    reference: &str,
    downloads: &Path,
    fresh: bool,
) -> Result<PathBuf, String> {
    let name = sha2::Sha256::digest(format!("{url}@{reference}").as_bytes());
    let name: String = name
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let folder = downloads.join("git").join("web").join(name);
    if folder.is_dir() && !fresh {
        return Ok(folder);
    }
    written(&folder, |target| (client.0)(url, reference, target))?;
    Ok(folder)
}

/// Writes a folder through a temporary one beside it, so that a folder is whole or absent.
fn written(folder: &Path, write: impl FnOnce(&Path) -> Result<(), String>) -> Result<(), String> {
    let parent = folder.parent().unwrap_or(folder);
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let name = folder.file_name().unwrap_or_default().to_string_lossy();
    let target = parent.join(format!(".{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&target);
    let result = write(&target).and_then(|()| {
        if folder.exists() {
            std::fs::remove_dir_all(folder).map_err(|error| error.to_string())?;
        }
        std::fs::rename(&target, folder).map_err(|error| error.to_string())
    });
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&target);
    }
    result
}

/// The commit that a tag, branch or commit ID names, as `git show <reference>:<path>` finds it.
fn commit_of(repository: &gix::Repository, reference: &str) -> Result<gix::ObjectId, String> {
    let missing = || format!("no tag, branch or commit {reference}");
    if reference.len() == 40
        && let Ok(id) = gix::ObjectId::from_hex(reference.as_bytes())
    {
        return Ok(id);
    }
    for name in [
        format!("refs/tags/{reference}"),
        format!("refs/heads/{reference}"),
        reference.to_owned(),
    ] {
        if let Ok(Some(mut found)) = repository.try_find_reference(name.as_str()) {
            let id = found.peel_to_id().map_err(|error| error.to_string())?;
            let object = id.object().map_err(|error| error.to_string())?;
            let commit = object.peel_to_commit().map_err(|_| missing())?;
            return Ok(commit.id);
        }
    }
    Err(missing())
}

/// Writes the files and links of a tree; submodules stay empty folders.
fn write_tree(
    repository: &gix::Repository,
    tree: gix::ObjectId,
    target: &Path,
) -> Result<(), String> {
    let failed = |error: std::io::Error| error.to_string();
    std::fs::create_dir_all(target).map_err(failed)?;
    let tree = (repository.find_tree(tree)).map_err(|error| error.to_string())?;
    for entry in tree.iter() {
        let entry = entry.map_err(|error| error.to_string())?;
        let name = (entry.filename().to_str()).map_err(|_| "a file name is no UTF-8")?;
        if matches!(name, "" | "." | ".." | ".git") || name.contains('/') {
            return Err(format!(
                "the repository holds an invalid file name {name:?}"
            ));
        }
        let path = target.join(name);
        let id = entry.oid().to_owned();
        match entry.mode().kind() {
            EntryKind::Tree => write_tree(repository, id, &path)?,
            EntryKind::Commit => std::fs::create_dir_all(&path).map_err(failed)?,
            kind => {
                let blob = (repository.find_blob(id)).map_err(|error| error.to_string())?;
                if kind == EntryKind::Link {
                    let link = blob.data.to_str().map_err(|_| "a link is no UTF-8")?;
                    std::os::unix::fs::symlink(link, &path).map_err(failed)?;
                    continue;
                }
                std::fs::write(&path, &blob.data).map_err(failed)?;
                if kind == EntryKind::BlobExecutable {
                    use std::os::unix::fs::PermissionsExt;
                    let mode = std::fs::Permissions::from_mode(0o755);
                    std::fs::set_permissions(&path, mode).map_err(failed)?;
                }
            }
        }
    }
    Ok(())
}
