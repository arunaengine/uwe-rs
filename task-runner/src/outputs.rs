//! Stores what a finished task made: each output under the attempt and by its content, and the
//! captured streams. A local store takes the files over from the scratch directory.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use content_store::{Store, layout};
use executor::Finished;
use task_model::{Digest, Tree, TreeEntry};

use crate::runner::at;
use crate::{RunnerError, StoredFile, TaskResult};

/// Stores the outputs of a finished attempt of `task` in the run `run`, and its captured
/// streams by their content.
pub(crate) async fn store_outputs(
    store: &Store,
    run: &str,
    finished: &Finished,
    task: &str,
    attempt: u32,
) -> Result<TaskResult, RunnerError> {
    let mut files = Vec::new();
    for collected in &finished.files {
        let local = finished.local_path(&collected.path);
        let path = &collected.path;
        let (location, digest, size) = if collected.directory {
            let place = Place { run, task, attempt };
            let (digest, size) = store_directory(store, &place, &local, path).await?;
            (layout::output(run, task, attempt, path)?, digest, size)
        } else {
            let stored = store.adopt_output(run, task, attempt, path, &local).await?;
            (stored.location, stored.digest, stored.size)
        };
        files.push(StoredFile {
            path: path.clone(),
            digest,
            size,
            location,
            directory: collected.directory,
        });
    }
    let log = async |file: PathBuf| -> Result<Option<Digest>, RunnerError> {
        if !tokio::fs::try_exists(&file).await.map_err(at(&file))? {
            return Ok(None);
        }
        Ok(Some(store.put_file(&file).await?.0))
    };
    let times = finished.tool_times();
    Ok(TaskResult {
        exit_code: finished.exit_code,
        files,
        stdout: log(finished.stdout()).await?,
        stderr: log(finished.stderr()).await?,
        reused: false,
        usage: finished.usage,
        started: times.map(|(started, _)| started),
        ended: times.map(|(_, ended)| ended),
        check: None,
    })
}

/// The attempt that outputs belong to.
struct Place<'a> {
    run: &'a str,
    task: &'a str,
    attempt: u32,
}

/// Stores a directory output at `base`: each file under the output and by its content, the
/// empty directories in a local store, and the manifest. The result has the digest of the
/// manifest and the size of the files.
async fn store_directory(
    store: &Store,
    place: &Place<'_>,
    directory: &Path,
    base: &str,
) -> Result<(Digest, u64), RunnerError> {
    let Place { run, task, attempt } = *place;
    if let Some(path) = store.local_path(&layout::output(run, task, attempt, base)?) {
        tokio::fs::create_dir_all(&path).await.map_err(at(&path))?;
    }
    let mut entries = Vec::new();
    for found in walk(directory).await? {
        let inside = format!("{base}/{}", found.relative());
        match found {
            Found::File {
                relative,
                path,
                executable,
            } => {
                let stored = store
                    .adopt_output(run, task, attempt, &inside, &path)
                    .await?;
                entries.push(TreeEntry::File {
                    path: relative,
                    digest: stored.digest,
                    size: stored.size,
                    executable,
                });
            }
            Found::EmptyDirectory { relative } => {
                let location = layout::output(run, task, attempt, &inside)?;
                if let Some(path) = store.local_path(&location) {
                    tokio::fs::create_dir_all(&path).await.map_err(at(&path))?;
                }
                entries.push(TreeEntry::EmptyDirectory { path: relative });
            }
        }
    }
    let tree = Tree { entries };
    let size = tree.size();
    Ok((store.put_tree(&tree).await?, size))
}

/// A file or an empty directory inside a directory.
pub(crate) enum Found {
    File {
        relative: String,
        path: PathBuf,
        executable: bool,
    },
    EmptyDirectory {
        relative: String,
    },
}

impl Found {
    /// The path relative to the directory.
    fn relative(&self) -> &str {
        match self {
            Found::File { relative, .. } | Found::EmptyDirectory { relative } => relative,
        }
    }
}

/// The files and empty directories below a directory, by their paths relative to it, found on
/// a thread that may block. A symbolic link to a file counts as the file; links to
/// directories are not followed.
pub(crate) async fn walk(root: &Path) -> Result<Vec<Found>, RunnerError> {
    let root = root.to_owned();
    tokio::task::spawn_blocking(move || walk_now(&root))
        .await
        .map_err(|error| std::io::Error::other(error.to_string()))?
}

fn walk_now(root: &Path) -> Result<Vec<Found>, RunnerError> {
    let mut found = Vec::new();
    let mut pending = vec![(root.to_owned(), String::new())];
    while let Some((directory, prefix)) = pending.pop() {
        let mut empty = true;
        for entry in std::fs::read_dir(&directory).map_err(at(&directory))? {
            let entry = entry?;
            empty = false;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| RunnerError::Name(entry.path()))?;
            let relative = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            if entry.file_type()?.is_dir() {
                pending.push((entry.path(), relative));
            } else if let Ok(metadata) = std::fs::metadata(entry.path())
                && metadata.is_file()
            {
                found.push(Found::File {
                    relative,
                    path: entry.path(),
                    executable: metadata.permissions().mode() & 0o111 != 0,
                });
            }
        }
        if empty && !prefix.is_empty() {
            found.push(Found::EmptyDirectory { relative: prefix });
        }
    }
    Ok(found)
}
