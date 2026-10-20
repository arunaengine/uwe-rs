//! Hashes or stages task inputs while preserving side-effect-free cache lookups.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

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
    pub(crate) snapshots: Snapshots,
}

/// The private copies of the inputs of a request, in `input-snapshots/<run>/` of the
/// downloads. They go with the last clone of the request; `iunctus clean` removes the copies
/// of runs that ended without removing them.
#[derive(Clone, Debug)]
pub(crate) struct Snapshots {
    folder: PathBuf,
    made: Arc<Mutex<Vec<tempfile::TempDir>>>,
}

impl Snapshots {
    pub(crate) fn new(settings: &Settings, run: &str) -> Snapshots {
        let run = crate::runner::segment(run);
        Snapshots {
            folder: settings.downloads.join("input-snapshots").join(run),
            made: Arc::default(),
        }
    }

    /// A new private folder that lives as long as these snapshots.
    pub(crate) async fn folder(&self) -> Result<PathBuf, RunnerError> {
        for folder in [self.folder.parent().expect("a parent"), &self.folder] {
            tokio::fs::create_dir_all(folder).await?;
            tokio::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o700)).await?;
        }
        let made = tempfile::tempdir_in(&self.folder)?;
        let path = made.path().to_owned();
        let mut all = self
            .made
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        all.push(made);
        Ok(path)
    }
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

    /// What a task reads of a local input: content that nothing changes any more where it is,
    /// else a private copy, so that the task reads the bytes that were hashed. A store that
    /// takes the input in copies those bytes itself.
    async fn private(
        &self,
        source: &Path,
        directory: bool,
        siblings: &BTreeMap<PathBuf, PathBuf>,
    ) -> Result<PathBuf, RunnerError> {
        let source = std::fs::canonicalize(source)?;
        if let Some(copy) = siblings.get(&source) {
            return Ok(copy.clone());
        }
        if self.imported() || self.immutable(&source) {
            return Ok(source);
        }
        self.snapshot(&source, directory, None).await
    }

    /// Whether nothing changes a file or directory any more: an output or content of the local
    /// store, or a download, which is complete when it gets its name and never written again.
    fn immutable(&self, path: &Path) -> bool {
        let inside = |root: &Path| {
            let root = std::fs::canonicalize(root).ok()?;
            path.strip_prefix(root).ok().map(Path::to_owned)
        };
        if let Some(location) = self.store.local_path("").and_then(|root| inside(&root)) {
            let kinds = ["runs", "cas", "directory-snapshots"];
            return kinds.iter().any(|kind| location.starts_with(kind));
        }
        inside(&self.settings.downloads).is_some_and(|location| {
            let parts: Vec<_> = location.iter().map(|part| part.to_string_lossy()).collect();
            matches!(parts.as_slice(), [bundle, file] if bundle.starts_with("v2-") && file == "content")
        })
    }

    /// A private copy that lives as long as the request; `destination` puts it in a folder
    /// that the caller made with [`Snapshots::folder`].
    async fn snapshot(
        &self,
        source: &Path,
        directory: bool,
        destination: Option<&Path>,
    ) -> Result<PathBuf, RunnerError> {
        if self.mode == Mode::Look {
            return Ok(std::fs::canonicalize(source)?);
        }
        let path = match destination {
            Some(path) => path.to_owned(),
            None => self.snapshots.folder().await?.join("input"),
        };
        if directory {
            tokio::fs::create_dir(&path).await?;
            for found in outputs::walk(source, outputs::Reach::Inside).await? {
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
        readable_snapshot(path.parent().expect("a snapshot parent")).await?;
        Ok(tokio::fs::canonicalize(path).await?)
    }

    /// Preserves source siblings using only declared inputs, with a key for their layout.
    pub(crate) async fn siblings(
        &self,
        inputs: &[Input],
    ) -> Result<(BTreeMap<PathBuf, PathBuf>, Option<Digest>), RunnerError> {
        let mut groups: BTreeMap<PathBuf, Vec<(usize, PathBuf)>> = BTreeMap::new();
        for (index, input) in inputs.iter().enumerate() {
            if input.content.is_some()
                || matches!(input.writable, Writable::InPlace | Writable::Shared)
                || input.source.contains("://")
                || !Path::new(&input.source).exists()
            {
                continue;
            }
            let source = std::fs::canonicalize(&input.source)?;
            if let Some(parent) = source.parent() {
                groups
                    .entry(parent.to_owned())
                    .or_default()
                    .push((index, source));
            }
        }
        let mut copies = BTreeMap::new();
        let mut layout = Vec::new();
        for members in groups.values().filter(|members| members.len() > 1) {
            layout.push(
                members
                    .iter()
                    .map(|(index, source)| {
                        (
                            *index,
                            source
                                .file_name()
                                .expect("a child")
                                .to_string_lossy()
                                .into_owned(),
                        )
                    })
                    .collect::<Vec<_>>(),
            );
            // Files that never change, or that go into the store, are read where they are.
            let fixed = members.iter().all(|(_, source)| self.immutable(source));
            if self.mode == Mode::Look || self.imported() || fixed {
                continue;
            }
            let parent = self.snapshots.folder().await?.join("input");
            tokio::fs::create_dir(&parent).await?;
            for (_, source) in members {
                if copies.contains_key(source) {
                    continue;
                }
                let target = parent.join(source.file_name().expect("a child"));
                let copy = self
                    .snapshot(source, source.is_dir(), Some(&target))
                    .await?;
                copies.insert(source.clone(), copy);
            }
        }
        layout.sort();
        let digest = (!layout.is_empty()).then(|| Digest::sha256(format!("{layout:?}").as_bytes()));
        Ok((copies, digest))
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
        for found in outputs::walk(directory, outputs::Reach::Inside).await? {
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
                let copy = (self.settings.storage)
                    .current_path(source, downloads)
                    .await?;
                match tokio::fs::try_exists(&copy).await {
                    Ok(true) => Ok(copy),
                    _ => Err(RunnerError::NotDownloaded(source.to_owned())),
                }
            }
        }
    }

    /// Materializes shared-only inputs into the local store for runs; cache lookups download
    /// nothing.
    async fn taken_into_folder(&self, input: &Input) -> Result<Option<Input>, RunnerError> {
        let shared = self.store.shared().is_some()
            && !matches!(input.writable, Writable::InPlace | Writable::Shared);
        let decoded = input
            .source
            .strip_prefix("file://")
            .map(storage::address::decode);
        let source = decoded.as_deref().unwrap_or(&input.source);
        let location = shared.then(|| self.store.location_of(source)).flatten();
        let Some(location) = location else {
            return Ok(None);
        };
        if location.starts_with("directory-snapshots/") {
            return Ok(None);
        }
        let path = match self.mode {
            Mode::Run => self.store.local_file(&location).await?,
            Mode::Look => match self.store.cached_directory(&location).await? {
                Some(digest) => self
                    .store
                    .local_path(&content_store::layout::directory_snapshot(&digest))
                    .map(|path| path.join("input"))
                    .filter(|path| path.is_dir()),
                None => self
                    .store
                    .local_path(&location)
                    .filter(|path| path.is_file()),
            },
        };
        let Some(path) = path else {
            return Ok(None);
        };
        if path.as_os_str() == std::ffi::OsStr::new(source) {
            return Ok(None);
        }
        Ok(Some(Input {
            source: path.to_string_lossy().into_owned(),
            ..input.clone()
        }))
    }

    /// An input by its digest, and where its content comes from: literal content, a local
    /// directory or a local file.
    pub(crate) async fn input(
        &self,
        input: &Input,
        siblings: &BTreeMap<PathBuf, PathBuf>,
    ) -> Result<(InputFile, Source), RunnerError> {
        if let Some(local) = self.taken_into_folder(input).await? {
            return Box::pin(self.input(&local, siblings)).await;
        }
        let source = Path::new(&input.source);
        if matches!(input.writable, Writable::InPlace | Writable::Shared) {
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
                let snapshot = self.private(&copy, false, siblings).await?;
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
                let snapshot = self.private(source, true, siblings).await?;
                (
                    FileKind::Directory,
                    self.directory(&snapshot).await?.0,
                    local(snapshot),
                )
            }
            None => {
                let snapshot = self.private(source, false, siblings).await?;
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

impl Inputs<'_> {
    /// A declared input that lies in a folder that the task sees in place: hashed where it
    /// is for the cache key, and staged by nothing.
    pub(crate) async fn present(&self, input: &Input) -> Result<(InputFile, Source), RunnerError> {
        let path = Path::new(&input.source);
        let metadata = tokio::fs::metadata(path).await.map_err(|error| {
            std::io::Error::new(error.kind(), format!("{}: {error}", path.display()))
        })?;
        let (kind, digest) = match metadata.is_dir() {
            true => (FileKind::Directory, self.directory(path).await?.0),
            false => (FileKind::File, self.file(path).await?.0),
        };
        let file = InputFile {
            path: input.path.clone(),
            kind,
            digest,
            executable: metadata.permissions().mode() & 0o111 != 0 && kind == FileKind::File,
            writable: false,
        };
        Ok((file, Source::Present))
    }
}

/// Whether an input already lies, as itself, in a folder that the task sees in place.
pub(crate) fn present(input: &Input, folders: &[PathBuf], workdir: Option<&str>) -> bool {
    if input.writable != Writable::No
        || input.content.is_some()
        || input.source.is_empty()
        || input.source.contains("://")
    {
        return false;
    }
    let target = match (Path::new(&input.path).is_absolute(), workdir) {
        (true, _) => PathBuf::from(&input.path),
        (false, Some(workdir)) => Path::new(workdir).join(&input.path),
        (false, None) => return false,
    };
    match (
        std::fs::canonicalize(&input.source),
        std::fs::canonicalize(&target),
    ) {
        (Ok(source), Ok(target)) => {
            source == target && folders.iter().any(|folder| target.starts_with(folder))
        }
        _ => false,
    }
}

/// Makes only private copies readable through container mounts, without following links.
pub(crate) async fn readable_snapshot(root: &Path) -> Result<(), RunnerError> {
    let mut pending = vec![root.to_owned()];
    while let Some(directory) = pending.pop() {
        tokio::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755)).await?;
        let mut entries = tokio::fs::read_dir(&directory).await?;
        while let Some(entry) = entries.next_entry().await? {
            let kind = entry.file_type().await?;
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                let executable = entry.metadata().await?.permissions().mode() & 0o111 != 0;
                let mode = if executable { 0o755 } else { 0o644 };
                tokio::fs::set_permissions(entry.path(), std::fs::Permissions::from_mode(mode))
                    .await?;
            }
        }
    }
    Ok(())
}
