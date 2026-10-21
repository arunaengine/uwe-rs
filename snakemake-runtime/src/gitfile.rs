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

/// The commit that a revision names, as `git show <revision>:<path>` finds it: a tag, a
/// branch, a commit ID or a short one, or an expression like `main~1`.
fn commit_of(repository: &gix::Repository, reference: &str) -> Result<gix::ObjectId, String> {
    let missing = || format!("no tag, branch or commit {reference}");
    let id = (repository.rev_parse_single(reference)).map_err(|_| missing())?;
    let object = id.object().map_err(|error| error.to_string())?;
    let commit = object.peel_to_commit().map_err(|_| missing())?;
    Ok(commit.id)
}

/// Writes the files and links of a tree; submodules stay empty folders. Links come last and
/// never lead out, so that no file is written through one.
fn write_tree(
    repository: &gix::Repository,
    tree: gix::ObjectId,
    target: &Path,
) -> Result<(), String> {
    std::fs::create_dir_all(target).map_err(|error| error.to_string())?;
    let mut links = Vec::new();
    write_files(repository, tree, target, Path::new(""), &mut links)?;
    for (relative, link) in &links {
        let path = real_parent(target, relative)?;
        std::os::unix::fs::symlink(link, path).map_err(|error| error.to_string())?;
    }
    contain_links(target, &links).map_err(|error| error.to_string())
}

/// Writes the folders and files of a tree below `inside`, and notes its links.
fn write_files(
    repository: &gix::Repository,
    tree: gix::ObjectId,
    target: &Path,
    inside: &Path,
    links: &mut Vec<(PathBuf, String)>,
) -> Result<(), String> {
    let failed = |error: std::io::Error| error.to_string();
    let tree = (repository.find_tree(tree)).map_err(|error| error.to_string())?;
    let mut names = std::collections::HashSet::new();
    for entry in tree.iter() {
        let entry = entry.map_err(|error| error.to_string())?;
        let name = (entry.filename().to_str()).map_err(|_| "a file name is no UTF-8")?;
        if matches!(name, "" | "." | ".." | ".git") || name.contains('/') {
            return Err(format!(
                "the repository holds an invalid file name {name:?}"
            ));
        }
        if !names.insert(name.to_owned()) {
            return Err(format!("the repository holds {name:?} twice in one folder"));
        }
        let relative = inside.join(name);
        let id = entry.oid().to_owned();
        match entry.mode().kind() {
            EntryKind::Tree => {
                std::fs::create_dir(real_parent(target, &relative)?).map_err(failed)?;
                write_files(repository, id, target, &relative, links)?;
            }
            EntryKind::Commit => {
                std::fs::create_dir(real_parent(target, &relative)?).map_err(failed)?
            }
            kind => {
                let blob = (repository.find_blob(id)).map_err(|error| error.to_string())?;
                if kind == EntryKind::Link {
                    let link = blob.data.to_str().map_err(|_| "a link is no UTF-8")?;
                    links.push((relative, link.to_owned()));
                    continue;
                }
                use std::io::Write;
                use std::os::unix::fs::OpenOptionsExt;
                let mode = match kind == EntryKind::BlobExecutable {
                    true => 0o755,
                    false => 0o644,
                };
                // `create_new` never follows a link that is there already.
                (std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(mode))
                .open(real_parent(target, &relative)?)
                .and_then(|mut file| file.write_all(&blob.data))
                .map_err(failed)?;
            }
        }
    }
    Ok(())
}

/// The path of `relative` below `target`, once each folder above it is a real folder there.
fn real_parent(target: &Path, relative: &Path) -> Result<PathBuf, String> {
    let mut folder = target.to_path_buf();
    let parents = relative.parent().into_iter().flat_map(Path::components);
    for part in parents {
        folder.push(part);
        let real = std::fs::symlink_metadata(&folder).is_ok_and(|meta| meta.is_dir());
        if !real {
            return Err(format!(
                "the repository writes {} below a link or a file",
                relative.display()
            ));
        }
    }
    Ok(target.join(relative))
}

/// Makes each link that leads out of the checkout a plain file that holds its target, as Git
/// does without `core.symlinks`; a link that leads out through another counts too.
fn contain_links(root: &Path, links: &[(PathBuf, String)]) -> std::io::Result<()> {
    let mut changed = true;
    while changed {
        changed = false;
        for (link, target) in links {
            let place = root.join(link);
            let linked = std::fs::symlink_metadata(&place).is_ok_and(|meta| meta.is_symlink());
            if !linked || resolved(root, link).is_some() {
                continue;
            }
            std::fs::remove_file(&place)?;
            std::fs::write(&place, target)?;
            changed = true;
        }
    }
    Ok(())
}

/// Where `relative` leads below `root` once its links are followed, or `None` when it leads
/// out of `root` or through too many links.
fn resolved(root: &Path, relative: &Path) -> Option<PathBuf> {
    let parts = |path: &Path| -> Vec<PathBuf> {
        (path.components().rev())
            .map(|part| PathBuf::from(part.as_os_str()))
            .collect()
    };
    let mut pending = parts(relative);
    let (mut current, mut hops) = (PathBuf::new(), 0);
    while let Some(part) = pending.pop() {
        match part.components().next()? {
            std::path::Component::Normal(name) => {
                let next = current.join(name);
                let place = root.join(&next);
                if !std::fs::symlink_metadata(&place).is_ok_and(|meta| meta.is_symlink()) {
                    current = next;
                    continue;
                }
                hops += 1;
                if hops > 40 {
                    return None;
                }
                pending.extend(parts(&std::fs::read_link(&place).ok()?));
            }
            std::path::Component::ParentDir => {
                if !current.pop() {
                    return None;
                }
            }
            std::path::Component::CurDir => {}
            _ => return None,
        }
    }
    Some(current)
}
