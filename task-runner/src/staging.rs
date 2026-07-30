//! What a backend places before a task starts: each input from where its content is.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;

use content_store::layout;
use executor::{Backend, Staged, Staging};
use task_model::{Digest, FileKind, TreeEntry};

use crate::runner::{Prepared, Source, at};
use crate::{Runner, RunnerError};

impl<B: Backend> Runner<B> {
    /// What the backend places for a request: a local input as itself, literal content as a
    /// copy, and what only the store has from the store, a directory as its files and empty
    /// directories.
    pub(crate) async fn stage(&self, prepared: &Prepared) -> Result<Staging, RunnerError> {
        let spec = &prepared.request.spec;
        let mut staging = Staging::default();
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
                Source::InPlace(path) => staging.inputs.push(Staged {
                    path: input.path.clone(),
                    source: path.clone(),
                    directory,
                    in_place: true,
                    ..Staged::default()
                }),
                Source::Content | Source::Stored if !directory => {
                    let content = self.local_content(&input.digest).await?;
                    let copy = *source == Source::Content;
                    staging.inputs.push(staged(content, copy));
                }
                Source::Content | Source::Stored => {
                    self.stage_tree(&input.path, &input.digest, &mut staging)
                        .await?;
                }
            }
        }
        if let Some(digest) = &spec.stdin {
            staging.stdin = Some(self.local_content(digest).await?);
        }
        Ok(staging)
    }

    /// Stages a directory that only the store has: its files and its empty directories.
    async fn stage_tree(
        &self,
        path: &str,
        digest: &Digest,
        staging: &mut Staging,
    ) -> Result<(), RunnerError> {
        let base = path.trim_end_matches('/');
        staging.directories.push(base.to_owned());
        let tree = self.store.get_tree(digest).await?;
        for entry in tree.entries {
            let relative = entry.path();
            let escapes = relative.starts_with('/') || relative.split('/').any(|part| part == "..");
            if relative.is_empty() || escapes {
                return Err(RunnerError::Tree(path.to_owned()));
            }
            let inside = format!("{base}/{relative}");
            match entry {
                TreeEntry::File {
                    digest, executable, ..
                } => staging.inputs.push(Staged {
                    path: inside,
                    source: self.local_content(&digest).await?,
                    directory: false,
                    copy: executable,
                    executable,
                    ..Default::default()
                }),
                TreeEntry::EmptyDirectory { .. } => staging.directories.push(inside),
            }
        }
        Ok(())
    }

    /// A local file with the content of a digest: the store's own file for a local store, else
    /// a download.
    async fn local_content(&self, digest: &Digest) -> Result<PathBuf, RunnerError> {
        if let Some(path) = self.store.local_path(&layout::content(digest)) {
            return Ok(path);
        }
        let downloads = &self.settings.downloads;
        let path = downloads.join(digest.hex());
        if !tokio::fs::try_exists(&path).await.map_err(at(&path))? {
            (tokio::fs::create_dir_all(downloads).await).map_err(at(downloads))?;
            let bytes = self.store.get_content(digest).await?;
            // A name of this process first, then a rename, so that another run that downloads
            // the same content never reads half a file.
            let part = downloads.join(format!("{}.{}", digest.hex(), std::process::id()));
            (tokio::fs::write(&part, bytes).await).map_err(at(&part))?;
            (tokio::fs::rename(&part, &path).await).map_err(at(&path))?;
        }
        Ok(path)
    }
}
