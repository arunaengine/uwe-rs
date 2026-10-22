//! What a backend places before a task starts: each input from where its content is.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use content_store::layout;
use executor::{Backend, Staged, Staging};
use task_model::{Digest, FileKind, TreeEntry};

use crate::inputs::Snapshots;
use crate::runner::{Prepared, Source, at};
use crate::{Runner, RunnerError};

/// The folder of the downloads with copies of contents of a store elsewhere, by digest. They
/// stay apart from copies of remote files, and clean-up removes the old ones.
pub const CONTENTS: &str = "contents";

impl<B: Backend> Runner<B> {
    /// What the backend places for a request: a local input as itself, literal content as a
    /// copy, and what only the store has from the store, a directory as its files and empty
    /// directories.
    pub(crate) async fn stage(&self, prepared: &Prepared) -> Result<Staging, RunnerError> {
        let spec = &prepared.request.spec;
        let mut staging = Staging::default();
        let mut verified = HashMap::new();
        for (input, source) in spec.inputs.iter().zip(&prepared.sources) {
            let directory = input.kind == FileKind::Directory;
            let staged = |source: PathBuf, copy: bool| Staged {
                path: input.path.clone(),
                source,
                directory,
                copy: copy || input.writable || input.executable,
                executable: input.executable,
                ..Default::default()
            };
            match source {
                Source::Local(path) => staging.inputs.push(staged(path.clone(), false)),
                // The folder that the task sees in place holds it already.
                Source::Present => {}
                Source::InPlace(path) => staging.inputs.push(Staged {
                    path: input.path.clone(),
                    source: path.clone(),
                    directory,
                    in_place: true,
                    ..Staged::default()
                }),
                Source::Content | Source::Stored if !directory => {
                    let content = self.stage_content(&input.digest, &mut verified).await?;
                    staging.inputs.push(staged(content, true));
                }
                Source::Content | Source::Stored => {
                    let snapshots = &prepared.snapshots;
                    self.stage_tree(
                        &input.path,
                        &input.digest,
                        input.writable,
                        snapshots,
                        &mut staging,
                        &mut verified,
                    )
                    .await?;
                }
            }
        }
        if let Some(digest) = &spec.stdin {
            staging.stdin = Some(self.stage_content(digest, &mut verified).await?);
        }
        Ok(staging)
    }

    /// Stages a directory that only the store has: its files and its empty directories. The
    /// task gets copies of the files when it may change them.
    async fn stage_tree(
        &self,
        path: &str,
        digest: &Digest,
        writable: bool,
        snapshots: &Snapshots,
        staging: &mut Staging,
        verified: &mut HashMap<Digest, PathBuf>,
    ) -> Result<(), RunnerError> {
        let base = path.trim_end_matches('/');
        let tree = self.store.get_tree(digest).await?;
        let linked = tree
            .entries
            .iter()
            .any(|entry| matches!(entry, TreeEntry::Symlink { .. }));
        let snapshot = if linked {
            Some(snapshots.folder().await?)
        } else {
            staging.directories.push(base.to_owned());
            None
        };
        for entry in tree.entries {
            let relative = entry.path().to_owned();
            let escapes = relative.starts_with('/') || relative.split('/').any(|part| part == "..");
            if relative.is_empty() || escapes {
                return Err(RunnerError::Tree(path.to_owned()));
            }
            let inside = format!("{base}/{relative}");
            if let Some(snapshot) = &snapshot {
                let target = snapshot.join(&relative);
                tokio::fs::create_dir_all(target.parent().expect("a parent")).await?;
                match entry {
                    TreeEntry::File {
                        digest, executable, ..
                    } => {
                        tokio::fs::copy(self.stage_content(&digest, verified).await?, &target)
                            .await?;
                        let mode = if executable { 0o755 } else { 0o644 };
                        tokio::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode))
                            .await?;
                    }
                    TreeEntry::EmptyDirectory { .. } => tokio::fs::create_dir_all(target).await?,
                    TreeEntry::Symlink { target: link, .. } => {
                        let mut depth = std::path::Path::new(&relative)
                            .parent()
                            .map_or(0, |parent| parent.components().count());
                        let internal =
                            std::path::Path::new(&link)
                                .components()
                                .all(|part| match part {
                                    std::path::Component::Normal(_) => {
                                        depth += 1;
                                        true
                                    }
                                    std::path::Component::ParentDir if depth > 0 => {
                                        depth -= 1;
                                        true
                                    }
                                    std::path::Component::CurDir => true,
                                    _ => false,
                                });
                        if !internal {
                            return Err(RunnerError::Tree(path.to_owned()));
                        }
                        tokio::fs::symlink(link, target).await?;
                    }
                }
                continue;
            }
            match entry {
                TreeEntry::File {
                    digest, executable, ..
                } => staging.inputs.push(Staged {
                    path: inside,
                    source: self.stage_content(&digest, verified).await?,
                    directory: false,
                    copy: true,
                    executable,
                    ..Default::default()
                }),
                TreeEntry::EmptyDirectory { .. } => staging.directories.push(inside),
                TreeEntry::Symlink { .. } => unreachable!("linked trees have a snapshot"),
            }
        }
        if let Some(snapshot) = snapshot {
            crate::inputs::readable_snapshot(&snapshot).await?;
            staging.inputs.push(Staged {
                path: base.to_owned(),
                source: std::fs::canonicalize(&snapshot)?,
                directory: true,
                copy: writable,
                ..Staged::default()
            });
        }
        Ok(())
    }

    /// Verifies repeated content once while staging an attempt; later attempts check again.
    async fn stage_content(
        &self,
        digest: &Digest,
        verified: &mut HashMap<Digest, PathBuf>,
    ) -> Result<PathBuf, RunnerError> {
        if let Some(path) = verified.get(digest) {
            return Ok(path.clone());
        }
        let path = self.local_content(digest).await?;
        verified.insert(digest.clone(), path.clone());
        Ok(path)
    }

    /// A local file with the content of a digest: the store's own file for a local store or a
    /// store in layers, else a copy in [`CONTENTS`] of the downloads, streamed in parts.
    pub(crate) async fn local_content(&self, digest: &Digest) -> Result<PathBuf, RunnerError> {
        let location = layout::content(digest);
        if let Some(path) = self.store.local_file(&location).await? {
            let (actual, _) = content_store::hash_file(&path).await?;
            if actual != *digest {
                let _ = self.store.repair(&location, digest).await;
                let expected = digest.clone();
                return Err(content_store::StoreError::Corrupt { expected, actual }.into());
            }
            return Ok(path);
        }
        // Clean-up removes no download while a process holds the folder.
        let downloads = &self.settings.downloads;
        self.settings.storage.hold(downloads).await?;
        let path = downloads.join(CONTENTS).join(digest.hex());
        if tokio::fs::try_exists(&path).await.map_err(at(&path))?
            && content_store::hash_file(&path).await?.0 == *digest
        {
            return Ok(path);
        }
        // A damaged copy is replaced by a new download, checked as it streams in.
        self.store.materialize_content(digest, &path).await?;
        Ok(path)
    }
}
