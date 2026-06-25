//! What a backend places before a task starts: each input from where its content is.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;

use content_store::layout;
use executor::{Backend, Staged, Staging};
use task_model::{Digest, FileKind, TaskSpec, TreeEntry};

use crate::{Runner, RunnerError};

impl<B: Backend> Runner<B> {
    /// What the backend places for a specification: each input file, the files and empty
    /// directories of each directory input, and the standard input.
    pub(crate) async fn stage(&self, spec: &TaskSpec) -> Result<Staging, RunnerError> {
        let mut staging = Staging::default();
        for input in &spec.inputs {
            if input.kind == FileKind::File {
                staging.inputs.push(Staged {
                    path: input.path.clone(),
                    source: self.local_content(&input.digest).await?,
                    directory: false,
                    copy: true,
                    executable: input.executable,
                });
                continue;
            }
            let base = input.path.trim_end_matches('/');
            staging.directories.push(base.to_owned());
            let tree = self.store.get_tree(&input.digest).await?;
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
                    } => staging.inputs.push(Staged {
                        path,
                        source: self.local_content(&digest).await?,
                        directory: false,
                        copy: true,
                        executable,
                    }),
                    TreeEntry::EmptyDirectory { .. } => staging.directories.push(path),
                }
            }
        }
        if let Some(digest) = &spec.stdin {
            staging.stdin = Some(self.local_content(digest).await?);
        }
        Ok(staging)
    }

    /// A local file with the content of a digest: the store's own file for a local store, else
    /// a download.
    async fn local_content(&self, digest: &Digest) -> Result<PathBuf, RunnerError> {
        if let Some(path) = self.store.local_path(&layout::content(digest)) {
            return Ok(path);
        }
        let path = self.downloads.join(digest.hex());
        if !tokio::fs::try_exists(&path).await? {
            tokio::fs::create_dir_all(&self.downloads).await?;
            let bytes = self.store.get_content(digest).await?;
            // A name of this process first, then a rename, so that another run that downloads
            // the same content never reads half a file.
            let part = self
                .downloads
                .join(format!("{}.{}", digest.hex(), std::process::id()));
            tokio::fs::write(&part, bytes).await?;
            tokio::fs::rename(&part, &path).await?;
        }
        Ok(path)
    }
}
