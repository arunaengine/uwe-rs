//! The inputs of a task by their digests. A run stores literal content and directory manifests,
//! and downloads remote inputs; a lookup of the cache only hashes, without writing anything or
//! using the network.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

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
    /// downloaded, and local inputs are stored too when the runner imports inputs.
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

    /// The digest and size of a local file.
    async fn file(&self, file: &Path) -> Result<(Digest, u64), RunnerError> {
        match self.imported() {
            true => Ok(self.store.put_file(file).await?),
            false => Ok(content_store::hash_file(file).await?),
        }
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

    /// An input by its digest, and where its content comes from: literal content, a local
    /// directory or a local file. A local path is taken with its links resolved, so that a
    /// container that sees the folder of the path sees the content. When the runner imports
    /// inputs, the backend takes them from the store instead.
    ///
    /// An input that the task changes where it is is named by its path, not its content,
    /// which the task changes: such a task never comes from the cache, and a large folder,
    /// like the working directory of a workflow, is not read.
    pub(crate) async fn input(&self, input: &Input) -> Result<(InputFile, Source), RunnerError> {
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
        let local = || -> Result<Source, RunnerError> {
            if imported {
                return Ok(Source::Stored);
            }
            Ok(Source::Local(std::fs::canonicalize(source).map_err(
                |error| std::io::Error::new(error.kind(), format!("{}: {error}", source.display())),
            )?))
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
                match imported {
                    true => (FileKind::File, self.file(&copy).await?.0, Source::Stored),
                    false => {
                        let (digest, _) = content_store::hash_file(&copy).await?;
                        (FileKind::File, digest, Source::Local(copy))
                    }
                }
            }
            // A directory that the workflow makes itself, like an empty CWL `Directory` literal.
            None if input.directory && input.source.is_empty() => (
                FileKind::Directory,
                self.tree(&Tree::default()).await?,
                Source::Stored,
            ),
            None if !source.exists() => return Err(RunnerError::Missing(source.to_owned())),
            None if input.directory || source.is_dir() => (
                FileKind::Directory,
                self.directory(source).await?.0,
                local()?,
            ),
            None => (FileKind::File, self.file(source).await?.0, local()?),
        };
        let file = InputFile {
            path: input.path.clone(),
            kind,
            digest,
            executable: false,
            writable: input.writable == Writable::Copy,
        };
        Ok((file, from))
    }
}
