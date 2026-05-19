//! One runner per run. It hands tasks to an executor one at a time and keeps the cache.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use bytes::Bytes;
use content_store::{Store, StoreError, layout};
use executor::{Executor, ExecutorError, Finished, Staged, Staging};
use serde::{Deserialize, Serialize};
use task_model::{Digest, FileKind, ReuseScope, SpecError, TaskRequest, TaskSpec, Tree, TreeEntry};

/// An output of a task result, in the store.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredFile {
    /// The path as the task named it: relative to the work directory, or absolute.
    pub path: String,
    /// The digest of the file, or of the manifest of a directory.
    pub digest: Digest,
    /// The size of the file, or of all files in the directory.
    pub size: u64,
    /// Where the store keeps it under the run that made it; for a directory, the prefix of
    /// its files.
    pub location: String,
    pub directory: bool,
}

/// What a task produced, now or in an earlier run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskResult {
    /// The tool's exit code; `None` when the task was killed before it recorded one.
    pub exit_code: Option<i32>,
    pub files: Vec<StoredFile>,
    /// The digests of the captured standard output and error, when the task got that far.
    pub stdout: Option<Digest>,
    pub stderr: Option<Digest>,
    /// Whether a stored result answered the request.
    pub reused: bool,
}

/// A cache entry: the outputs of a successful execution, and the run and task that made them.
#[derive(Serialize, Deserialize)]
struct Entry {
    run: String,
    task: String,
    files: Vec<StoredFile>,
    stdout: Option<Digest>,
    stderr: Option<Digest>,
}

#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Executor(#[from] ExecutorError),
    #[error(transparent)]
    Spec(#[from] SpecError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("cannot read the cache entry {key}: {message}")]
    Entry { key: Digest, message: String },
    #[error("remote input files are not supported yet: {0}")]
    Remote(String),
    #[error("the name of `{0}` is not UTF-8")]
    Name(PathBuf),
    #[error("the directory manifest of the input `{0}` names a path outside the directory")]
    Tree(String),
}

impl RunnerError {
    /// Whether the cause is a feature that the runner does not support yet.
    pub fn unsupported(&self) -> bool {
        matches!(self, RunnerError::Remote(_))
    }
}

pub struct Runner<E> {
    store: Store,
    executor: E,
    runtime: tokio::runtime::Runtime,
    run: String,
    /// Where inputs of a remote store are downloaded before a task.
    downloads: PathBuf,
}

impl<E: Executor> Runner<E> {
    /// A runner for the run `run`, whose outputs go to `runs/<run>/` in the store.
    pub fn new(
        store: Store,
        executor: E,
        run: &str,
        downloads: &Path,
    ) -> Result<Self, RunnerError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        Ok(Self {
            store,
            executor,
            runtime,
            run: run.to_owned(),
            downloads: downloads.to_owned(),
        })
    }

    /// Stores a local file by its content, for tasks that take it as input.
    pub fn import(&self, file: &Path) -> Result<(Digest, u64), RunnerError> {
        Ok(self.runtime.block_on(self.store.put_file(file))?)
    }

    /// Stores bytes by their content, like a task script.
    pub fn import_bytes(&self, bytes: &[u8]) -> Result<Digest, RunnerError> {
        Ok(self
            .runtime
            .block_on(self.store.put_content(Bytes::copy_from_slice(bytes)))?)
    }

    /// Stores the files of a local directory by their content, and the manifest of the
    /// directory; the result has the digest of the manifest and the size of the files.
    pub fn import_directory(&self, directory: &Path) -> Result<(Digest, u64), RunnerError> {
        self.put_directory(directory, None)
    }

    /// Stores a directory manifest, like the one of an empty directory.
    pub fn import_tree(&self, tree: &Tree) -> Result<Digest, RunnerError> {
        Ok(self.runtime.block_on(self.store.put_tree(tree))?)
    }

    /// Reads an object of the store, like a stored output.
    pub fn read(&self, location: &str) -> Result<Bytes, RunnerError> {
        Ok(self.runtime.block_on(self.store.get_object(location))?)
    }

    /// Reads content by its digest, like a captured standard output.
    pub fn read_content(&self, digest: &Digest) -> Result<Bytes, RunnerError> {
        Ok(self.runtime.block_on(self.store.get_content(digest))?)
    }

    /// The executor that runs the tasks.
    pub fn executor(&self) -> &E {
        &self.executor
    }

    /// Where an object is on disk, for a local store.
    pub fn local_path(&self, location: &str) -> Option<PathBuf> {
        self.store.local_path(location)
    }

    /// Runs a request, unless the cache has a result for it that the reuse scope allows.
    pub fn execute(&self, request: &TaskRequest) -> Result<TaskResult, RunnerError> {
        let key = request.spec.cache_key()?;
        let namespace = request.spec.namespace.as_str();
        if request.scope == ReuseScope::Shared
            && let Some(bytes) = self
                .runtime
                .block_on(self.store.get_entry(namespace, &key))?
        {
            let entry: Entry =
                serde_json::from_slice(&bytes).map_err(|error| RunnerError::Entry {
                    key: key.clone(),
                    message: error.to_string(),
                })?;
            return Ok(TaskResult {
                exit_code: Some(0),
                files: entry.files,
                stdout: entry.stdout,
                stderr: entry.stderr,
                reused: true,
            });
        }
        let task = segment(&request.identity);
        let staging = self.stage(&request.spec)?;
        let name = format!("{}-{task}-{}", self.run, request.attempt);
        let finished = self.executor.run(&name, &request.spec, &staging)?;
        let stored = self.store_outputs(&finished, &task, request.attempt);
        std::fs::remove_dir_all(&finished.root)?;
        let result = stored?;
        if result.exit_code == Some(0) && request.scope != ReuseScope::None {
            let entry = Entry {
                run: self.run.clone(),
                task,
                files: result.files.clone(),
                stdout: result.stdout.clone(),
                stderr: result.stderr.clone(),
            };
            let bytes = serde_json::to_vec(&entry).map_err(|error| RunnerError::Entry {
                key: key.clone(),
                message: error.to_string(),
            })?;
            self.runtime
                .block_on(self.store.put_entry(namespace, &key, bytes.into()))?;
        }
        Ok(result)
    }

    /// What the executor places for a specification: each input file, the files and empty
    /// directories of each directory input, and the standard input.
    fn stage(&self, spec: &TaskSpec) -> Result<Staging, RunnerError> {
        let mut staging = Staging::default();
        for input in &spec.inputs {
            if input.kind == FileKind::File {
                staging.files.push(Staged {
                    path: input.path.clone(),
                    source: self.local_content(&input.digest)?,
                    executable: input.executable,
                });
                continue;
            }
            let base = input.path.trim_end_matches('/');
            staging.directories.push(base.to_owned());
            let tree = self.runtime.block_on(self.store.get_tree(&input.digest))?;
            for entry in tree.entries {
                let path = entry.path();
                let escapes = path.starts_with('/') || path.split('/').any(|part| part == "..");
                if path.is_empty() || escapes {
                    return Err(RunnerError::Tree(input.path.clone()));
                }
                let path = format!("{base}/{path}");
                match entry {
                    TreeEntry::File {
                        digest, executable, ..
                    } => staging.files.push(Staged {
                        path,
                        source: self.local_content(&digest)?,
                        executable,
                    }),
                    TreeEntry::EmptyDirectory { .. } => staging.directories.push(path),
                }
            }
        }
        if let Some(digest) = &spec.stdin {
            staging.stdin = Some(self.local_content(digest)?);
        }
        Ok(staging)
    }

    /// A local file with the content of a digest: the store's own file for a local store, else
    /// a download.
    fn local_content(&self, digest: &Digest) -> Result<PathBuf, RunnerError> {
        if let Some(path) = self.store.local_path(&layout::content(digest)) {
            return Ok(path);
        }
        let path = self.downloads.join(digest.hex());
        if !path.exists() {
            std::fs::create_dir_all(&self.downloads)?;
            let bytes = self.runtime.block_on(self.store.get_content(digest))?;
            // A name of this process first, then a rename, so that another run that downloads
            // the same content never reads half a file.
            let part = self
                .downloads
                .join(format!("{}.{}", digest.hex(), std::process::id()));
            std::fs::write(&part, bytes)?;
            std::fs::rename(&part, &path)?;
        }
        Ok(path)
    }

    /// Stores the outputs of a finished task under its attempt, reading each file in parts,
    /// and the captured streams by their content.
    fn store_outputs(
        &self,
        finished: &Finished,
        task: &str,
        attempt: u32,
    ) -> Result<TaskResult, RunnerError> {
        let mut files = Vec::new();
        for collected in &finished.files {
            let local = finished.local_path(&collected.path);
            let location = layout::output(&self.run, task, attempt, &collected.path)?;
            let (digest, size) = if collected.directory {
                if let Some(path) = self.store.local_path(&location) {
                    std::fs::create_dir_all(path)?;
                }
                self.put_directory(&local, Some((task, attempt, &collected.path)))?
            } else {
                self.runtime.block_on(self.store.put_output_file(
                    &self.run,
                    task,
                    attempt,
                    &collected.path,
                    &local,
                ))?;
                self.runtime.block_on(self.store.put_file(&local))?
            };
            files.push(StoredFile {
                path: collected.path.clone(),
                digest,
                size,
                location,
                directory: collected.directory,
            });
        }
        let log = |file: PathBuf| -> Result<Option<Digest>, RunnerError> {
            if !file.is_file() {
                return Ok(None);
            }
            Ok(Some(self.runtime.block_on(self.store.put_file(&file))?.0))
        };
        Ok(TaskResult {
            exit_code: finished.exit_code,
            files,
            stdout: log(finished.stdout())?,
            stderr: log(finished.stderr())?,
            reused: false,
        })
    }

    /// Stores the files of a local directory by their content and returns the digest of its
    /// manifest and the size of its files. With an output, given as the task, the attempt and
    /// the path of the directory output, each file is also stored under that output, and a
    /// local store gets the empty directories too, so that the directory is whole on disk.
    fn put_directory(
        &self,
        directory: &Path,
        output: Option<(&str, u32, &str)>,
    ) -> Result<(Digest, u64), RunnerError> {
        let mut entries = Vec::new();
        for found in walk(directory)? {
            match found {
                Found::File {
                    relative,
                    path,
                    executable,
                } => {
                    let (digest, size) = self.runtime.block_on(self.store.put_file(&path))?;
                    if let Some((task, attempt, base)) = output {
                        let inside = format!("{base}/{relative}");
                        self.runtime.block_on(
                            self.store
                                .put_output_file(&self.run, task, attempt, &inside, &path),
                        )?;
                    }
                    entries.push(TreeEntry::File {
                        path: relative,
                        digest,
                        size,
                        executable,
                    });
                }
                Found::EmptyDirectory { relative } => {
                    if let Some((task, attempt, base)) = output {
                        let inside = format!("{base}/{relative}");
                        let location = layout::output(&self.run, task, attempt, &inside)?;
                        if let Some(path) = self.store.local_path(&location) {
                            std::fs::create_dir_all(path)?;
                        }
                    }
                    entries.push(TreeEntry::EmptyDirectory { path: relative });
                }
            }
        }
        let tree = Tree { entries };
        let size = tree.size();
        Ok((self.runtime.block_on(self.store.put_tree(&tree))?, size))
    }
}

/// A file or an empty directory inside a directory.
enum Found {
    File {
        relative: String,
        path: PathBuf,
        executable: bool,
    },
    EmptyDirectory {
        relative: String,
    },
}

/// The files and empty directories below a directory, by their paths relative to it. A
/// symbolic link to a file counts as the file; links to directories are not followed.
fn walk(root: &Path) -> Result<Vec<Found>, RunnerError> {
    let mut found = Vec::new();
    let mut pending = vec![(root.to_owned(), String::new())];
    while let Some((directory, prefix)) = pending.pop() {
        let mut empty = true;
        for entry in std::fs::read_dir(&directory)? {
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

/// A task identity as one path segment of the store.
pub(crate) fn segment(identity: &str) -> String {
    identity
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect()
}
