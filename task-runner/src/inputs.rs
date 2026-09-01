//! The inputs of a task by their digests. A run stores literal content and directory manifests,
//! and downloads remote inputs; a lookup of the cache only hashes, without writing anything or
//! using the network.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use bytes::Bytes;
use content_store::Store;
use task_model::{Digest, FileKind, InputFile, Tree, TreeEntry};
use workflow_graph::{Input, Writable};

use crate::outputs::{self, Found};
use crate::runner::Source;
use crate::{RunnerError, Settings};

/// What the digests of inputs are made for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    /// A run: literal content and directory manifests go into the store, remote inputs are
    /// downloaded, and local inputs are copied before hashing or importing.
    Run,
    /// A lookup of the cache: digests only. A remote input that is not downloaded yet cannot
    /// be known.
    Look,
}

/// Makes the digests of the inputs of tasks.
pub(crate) struct Inputs<'a> {
    pub(crate) store: &'a Store,
    pub(crate) settings: &'a Settings,
    pub(crate) mode: Mode,
}

impl Inputs<'_> {
    /// Whether local inputs go into the store too, as tasks on another machine need.
    fn imported(&self) -> bool {
        self.mode == Mode::Run && self.settings.import_inputs
    }

    /// The digest of literal content, like a task script.
    pub(crate) async fn bytes(&self, bytes: &[u8]) -> Result<Digest, RunnerError> {
        match self.mode {
            Mode::Run => Ok(self
                .store
                .put_content(Bytes::copy_from_slice(bytes))
                .await?),
            Mode::Look => Ok(Digest::sha256(bytes)),
        }
    }

    /// The digest and size of a local file, which goes into the store too when the runner
    /// imports inputs.
    async fn file(&self, file: &Path) -> Result<(Digest, u64), RunnerError> {
        if self.imported() {
            return Ok(self.store.put_file(file).await?);
        }
        self.hash(file).await
    }

    /// The digest and size of a local file: from the cache of digests when the file did not
    /// change since a run read it, else read now.
    async fn hash(&self, file: &Path) -> Result<(Digest, u64), RunnerError> {
        match &self.settings.digests {
            Some(digests) => Ok(digests.hash(file).await?),
            None => Ok(content_store::hash_file(file).await?),
        }
    }

    /// A persistent private copy: remote jobs may still use it after preparation is dropped.
    async fn snapshot(&self, source: &Path, directory: bool) -> Result<PathBuf, RunnerError> {
        if self.mode == Mode::Look {
            return Ok(std::fs::canonicalize(source)?);
        }
        let root = self.settings.downloads.join("input-snapshots");
        tokio::fs::create_dir_all(&root).await?;
        let snapshot = tempfile::tempdir_in(&root)?;
        tokio::fs::set_permissions(snapshot.path(), std::fs::Permissions::from_mode(0o755)).await?;
        let path = snapshot.path().join("input");
        if directory {
            tokio::fs::create_dir(&path).await?;
            for found in outputs::walk(source).await? {
                match found {
                    Found::File {
                        relative,
                        path: source,
                        ..
                    } => {
                        let target = path.join(relative);
                        tokio::fs::create_dir_all(target.parent().expect("a parent")).await?;
                        tokio::fs::copy(&source, &target).await?;
                    }
                    Found::EmptyDirectory { relative } => {
                        tokio::fs::create_dir_all(path.join(relative)).await?;
                    }
                    Found::Symlink { relative, target } => {
                        let link = path.join(relative);
                        tokio::fs::create_dir_all(link.parent().expect("a parent")).await?;
                        tokio::fs::symlink(target, link).await?;
                    }
                }
            }
        } else {
            tokio::fs::copy(source, &path).await?;
        }
        let path = tokio::fs::canonicalize(path).await?;
        let _ = snapshot.keep();
        Ok(path)
    }

    /// The digest of a directory manifest, like the one of an empty directory.
    pub(crate) async fn tree(&self, tree: &Tree) -> Result<Digest, RunnerError> {
        match self.mode {
            Mode::Run => Ok(self.store.put_tree(tree).await?),
            Mode::Look => Ok(tree.digest()?),
        }
    }

    /// The digest of the manifest of a local directory, and the size of its files.
    pub(crate) async fn directory(&self, directory: &Path) -> Result<(Digest, u64), RunnerError> {
        let mut entries = Vec::new();
        for found in outputs::walk(directory).await? {
            entries.push(match found {
                Found::File {
                    relative,
                    path,
                    executable,
                } => {
                    let (digest, size) = self.file(&path).await?;
                    TreeEntry::File {
                        path: relative,
                        digest,
                        size,
                        executable,
                    }
                }
                Found::EmptyDirectory { relative } => TreeEntry::EmptyDirectory { path: relative },
                Found::Symlink { relative, target } => TreeEntry::Symlink {
                    path: relative,
                    target,
                },
            });
        }
        let tree = Tree { entries };
        let size = tree.size();
        Ok((self.tree(&tree).await?, size))
    }

    /// The local copy of a remote file: downloaded once for a run; for a lookup only one that
    /// an earlier run downloaded.
    async fn remote(&self, source: &str) -> Result<PathBuf, RunnerError> {
        let downloads = &self.settings.downloads;
        match self.mode {
            Mode::Run => Ok(self.settings.storage.fetch(source, downloads).await?),
            Mode::Look => {
                let copy = downloads.join(storage::download_name(source));
                match tokio::fs::try_exists(&copy).await {
                    Ok(true) => Ok(copy),
                    _ => Err(RunnerError::NotDownloaded(source.to_owned())),
                }
            }
        }
    }

    /// The input with a file of this machine in place of an object that only the shared store
    /// of a store in layers has, like an output of a task of another machine: a run takes the
    /// object into the folder of the store first, as it does with everything it reads there.
    /// A lookup of the cache downloads nothing.
    async fn taken_into_folder(&self, input: &Input) -> Result<Option<Input>, RunnerError> {
        let shared = self.store.shared().is_some() && self.mode == Mode::Run;
        let location = (shared && input.source.contains("://"))
            .then(|| self.store.location_of(&input.source))
            .flatten();
        let Some(location) = location else {
            return Ok(None);
        };
        let Some(path) = self.store.local_file(&location).await? else {
            return Ok(None);
        };
        Ok(Some(Input {
            source: path.to_string_lossy().into_owned(),
            ..input.clone()
        }))
    }

    /// An input by its digest, and where its content comes from: literal content, a local
    /// directory or a local file. Local bytes are copied with links resolved, so that the
    /// backend executes the same bytes that were hashed. When the runner imports
    /// inputs, the backend takes them from the store instead.
    ///
    /// An input that the task changes where it is is named by its path, not its content,
    /// which the task changes: such a task never comes from the cache, and a large folder,
    /// like the working directory of a workflow, is not read.
    pub(crate) async fn input(&self, input: &Input) -> Result<(InputFile, Source), RunnerError> {
        if let Some(local) = self.taken_into_folder(input).await? {
            return Box::pin(self.input(&local)).await;
        }
        let source = Path::new(&input.source);
        if input.writable == Writable::InPlace {
            if input.source.contains("://") || input.content.is_some() {
                return Err(RunnerError::NotInPlace(input.source.clone()));
            }
            let place = std::fs::canonicalize(source).map_err(|error| {
                std::io::Error::new(error.kind(), format!("{}: {error}", source.display()))
            })?;
            let kind = match place.is_dir() {
                true => FileKind::Directory,
                false => FileKind::File,
            };
            let file = InputFile {
                path: input.path.clone(),
                kind,
                digest: Digest::sha256(place.to_string_lossy().as_bytes()),
                executable: false,
                writable: true,
            };
            return Ok((file, Source::InPlace(place)));
        }
        let imported = self.imported();
        let mut executable = input.executable;
        let local = |path| {
            if imported {
                Source::Stored
            } else {
                Source::Local(path)
            }
        };
        let (kind, digest, from) = match &input.content {
            Some(content) => (
                FileKind::File,
                self.bytes(content.as_bytes()).await?,
                Source::Content,
            ),
            None if input.source.contains("://") => {
                if input.directory {
                    return Err(RunnerError::Remote(input.source.clone()));
                }
                let copy = self.remote(&input.source).await?;
                let snapshot = self.snapshot(&copy, false).await?;
                executable |= input.writable != Writable::Copy
                    && tokio::fs::metadata(&snapshot).await?.permissions().mode() & 0o111 != 0;
                (
                    FileKind::File,
                    self.file(&snapshot).await?.0,
                    local(snapshot),
                )
            }
            // A directory that the workflow makes itself, like an empty CWL `Directory` literal.
            None if input.directory && input.source.is_empty() => (
                FileKind::Directory,
                self.tree(&Tree::default()).await?,
                Source::Stored,
            ),
            None if !source.exists() => return Err(RunnerError::Missing(source.to_owned())),
            None if input.directory || source.is_dir() => {
                let snapshot = self.snapshot(source, true).await?;
                (
                    FileKind::Directory,
                    self.directory(&snapshot).await?.0,
                    local(snapshot),
                )
            }
            None => {
                let snapshot = self.snapshot(source, false).await?;
                executable |= input.writable != Writable::Copy
                    && tokio::fs::metadata(&snapshot).await?.permissions().mode() & 0o111 != 0;
                (
                    FileKind::File,
                    self.file(&snapshot).await?.0,
                    local(snapshot),
                )
            }
        };
        let file = InputFile {
            path: input.path.clone(),
            kind,
            digest,
            executable,
            writable: input.writable == Writable::Copy,
        };
        Ok((file, from))
    }
}
