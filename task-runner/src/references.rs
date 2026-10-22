//! Verifies immutable local references and rejects changed installations before reuse.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use task_model::{Digest, FileKind, Tree, TreeEntry};

use crate::digests::Version;
use crate::outputs::{self, Found, Reach};
use crate::{RunnerError, runner::at};

/// Content-verified installations whose files and directories are read-only.
#[derive(Clone, Debug, Default)]
pub struct References {
    entries: BTreeMap<PathBuf, Arc<Reference>>,
}

#[derive(Debug)]
struct Reference {
    root: PathBuf,
    digest: Digest,
    kind: FileKind,
    members: BTreeMap<PathBuf, Digest>,
    tree: Option<Tree>,
    directories: BTreeMap<PathBuf, OnceLock<Subtree>>,
    stamps: BTreeMap<PathBuf, (Version, u32)>,
}

#[derive(Debug)]
struct Subtree {
    digest: Digest,
    tree: Tree,
}

impl References {
    pub(crate) fn tree(&self, digest: &Digest) -> Option<&Tree> {
        self.entries.values().find_map(|reference| {
            if reference.digest == *digest {
                reference.tree.as_ref()
            } else {
                reference
                    .directories
                    .values()
                    .filter_map(OnceLock::get)
                    .find(|subtree| subtree.digest == *digest)
                    .map(|subtree| &subtree.tree)
            }
        })
    }

    pub(crate) fn contains(&self, path: &Path) -> bool {
        path.ancestors().any(|parent| {
            self.entries
                .get(parent)
                .is_some_and(|reference| reference.stamps.contains_key(path))
        })
    }

    /// Registers exactly one file or directory by its expected content or tree digest.
    /// A changed installation must be registered again with its expected release digest.
    pub async fn register(&mut self, path: &Path, expected: Digest) -> Result<(), RunnerError> {
        let named = std::path::absolute(path).map_err(at(path))?;
        let path = tokio::fs::canonicalize(path).await.map_err(at(path))?;
        let stamps = captured(&path).await?;
        if let Some(reference) = self.entries.get(&path)
            && reference.digest == expected
            && reference.stamps == stamps
        {
            self.entries.insert(named, reference.clone());
            return Ok(());
        }
        let mut reference = Reference::read(path.clone()).await?;
        if reference.digest != expected {
            return Err(content_store::StoreError::Corrupt {
                expected,
                actual: reference.digest,
            }
            .into());
        }
        if stamps != captured(&path).await? {
            return Err(changed(&path));
        }
        reference.directories = stamps
            .iter()
            .filter(|(_, (_, mode))| *mode & 0o170000 == 0o040000)
            .map(|(path, _)| (path.clone(), OnceLock::new()))
            .collect();
        reference.stamps = stamps;
        let reference = Arc::new(reference);
        self.entries.insert(named, reference.clone());
        self.entries.insert(path, reference);
        Ok(())
    }

    pub(crate) async fn find(
        &self,
        path: &Path,
    ) -> Result<Option<(PathBuf, Digest, FileKind)>, RunnerError> {
        if self.entries.is_empty() {
            return Ok(None);
        }
        let named = std::path::absolute(path).map_err(at(path))?;
        let canonical = std::fs::canonicalize(path);
        let exact = self.entries.get(&named);
        let reference = exact
            .or_else(|| {
                named
                    .ancestors()
                    .find_map(|parent| self.entries.get(parent))
            })
            .or_else(|| {
                canonical
                    .as_ref()
                    .ok()
                    .and_then(|path| path.ancestors().find_map(|parent| self.entries.get(parent)))
            });
        let Some(reference) = reference else {
            return Ok(None);
        };
        let path = canonical.map_err(at(path))?;
        if (exact.is_some() && path != reference.root) || !path.starts_with(&reference.root) {
            return Err(changed(&named));
        }
        if captured(&reference.root).await? != reference.stamps {
            return Err(changed(&path));
        }
        if path == reference.root {
            Ok(Some((path, reference.digest.clone(), reference.kind)))
        } else if let Some(directory) = reference.directories.get(&path) {
            if directory.get().is_none() {
                let _ = directory.set(reference.subtree(&path)?);
            }
            let subtree = directory.get().expect("a verified reference subtree");
            Ok(Some((path, subtree.digest.clone(), FileKind::Directory)))
        } else {
            Ok(reference
                .members
                .get(&path)
                .map(|digest| (path.clone(), digest.clone(), FileKind::File)))
        }
    }
}

/// Hashes a candidate installation using the same file and tree encoding as registration.
/// Candidates may be writable; registration separately enforces immutable input access.
pub async fn reference_digest(path: &Path) -> Result<Digest, RunnerError> {
    let path = tokio::fs::canonicalize(path).await.map_err(at(path))?;
    Ok(Reference::read(path).await?.digest)
}

impl Reference {
    async fn read(path: PathBuf) -> Result<Self, RunnerError> {
        if !path.is_dir() && !path.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "reference path must be a file or directory",
            )
            .into());
        }
        let kind = if path.is_dir() {
            FileKind::Directory
        } else {
            FileKind::File
        };
        let mut members = BTreeMap::new();
        let mut tree = None;
        let digest = match kind {
            FileKind::File => content_store::hash_file(&path).await?.0,
            FileKind::Directory => {
                let mut entries = Vec::new();
                for found in outputs::walk(&path, Reach::Inside).await? {
                    entries.push(match found {
                        Found::File {
                            relative,
                            path,
                            executable,
                        } => {
                            let (digest, size) = content_store::hash_file(&path).await?;
                            members.insert(
                                std::fs::canonicalize(&path).map_err(at(&path))?,
                                digest.clone(),
                            );
                            TreeEntry::File {
                                path: relative,
                                digest,
                                size,
                                executable,
                            }
                        }
                        Found::EmptyDirectory { relative } => {
                            TreeEntry::EmptyDirectory { path: relative }
                        }
                        Found::Symlink { relative, target } => TreeEntry::Symlink {
                            path: relative,
                            target,
                        },
                    });
                }
                let manifest = Tree { entries };
                let digest = manifest.digest()?;
                tree = Some(manifest);
                digest
            }
        };
        Ok(Self {
            root: path,
            digest,
            kind,
            members,
            tree,
            directories: BTreeMap::new(),
            stamps: BTreeMap::new(),
        })
    }

    fn subtree(&self, path: &Path) -> Result<Subtree, RunnerError> {
        let relative = path.strip_prefix(&self.root).map_err(|_| changed(path))?;
        let mut entries = Vec::new();
        for entry in &self.tree.as_ref().expect("a registered directory").entries {
            let Ok(relative) = Path::new(entry.path()).strip_prefix(relative) else {
                continue;
            };
            if relative.as_os_str().is_empty() {
                continue;
            }
            if outputs::resolved(path, relative).is_none() {
                return Err(changed(path));
            }
            let mut entry = entry.clone();
            match &mut entry {
                TreeEntry::File { path, .. }
                | TreeEntry::EmptyDirectory { path }
                | TreeEntry::Symlink { path, .. } => {
                    *path = relative.to_str().expect("a verified tree path").to_owned();
                }
            }
            entries.push(entry);
        }
        let tree = Tree { entries };
        Ok(Subtree {
            digest: tree.digest()?,
            tree,
        })
    }
}

async fn captured(root: &Path) -> Result<BTreeMap<PathBuf, (Version, u32)>, RunnerError> {
    let root = root.to_owned();
    tokio::task::spawn_blocking(move || capture(&root))
        .await
        .map_err(|error| std::io::Error::other(error.to_string()))?
}

fn capture(root: &Path) -> Result<BTreeMap<PathBuf, (Version, u32)>, RunnerError> {
    let mut stamps = BTreeMap::new();
    let mut pending = vec![root.to_owned()];
    while let Some(path) = pending.pop() {
        let metadata = std::fs::symlink_metadata(&path).map_err(at(&path))?;
        let mode = metadata.permissions().mode();
        if !metadata.is_symlink()
            && mode & 0o222 != 0
            && !rustix::fs::statvfs(&path)
                .map_err(|error| at(&path)(error.into()))?
                .f_flag
                .contains(rustix::fs::StatVfsMountFlags::RDONLY)
        {
            return Err(RunnerError::File {
                path,
                error: std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "registered references must be read-only",
                ),
            });
        }
        if metadata.is_dir() {
            for entry in std::fs::read_dir(&path).map_err(at(&path))? {
                pending.push(entry?.path());
            }
        } else if !metadata.is_file() && !metadata.is_symlink() {
            return Err(changed(&path));
        }
        stamps.insert(path, (Version::of(&metadata), mode));
    }
    Ok(stamps)
}

fn changed(path: &Path) -> RunnerError {
    RunnerError::File {
        path: path.to_owned(),
        error: std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "registered reference changed; verify its release before reuse",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hashes_candidate_installation() {
        let root = tempfile::tempdir().unwrap();
        let empty = root.path().join("empty");
        std::fs::create_dir(&empty).unwrap();
        let file = root.path().join("genome");
        std::fs::write(&file, b"ACGT").unwrap();
        assert_eq!(
            reference_digest(&file).await.unwrap().as_str(),
            "sha256:1dff3e84fe7877e0673b69bbddcf40124e396e3f9943dd890c91b6a09adb9af0"
        );
        let digest = reference_digest(root.path()).await.unwrap();
        assert_eq!(
            digest.as_str(),
            "sha256:2863675a732d85e993d0b02bd60afe001c168e9e63abea2048fa16d63eb3f47b"
        );
        let mut references = References::default();
        assert!(
            references
                .register(root.path(), digest.clone())
                .await
                .is_err()
        );
        for (path, mode) in [
            (file.as_path(), 0o444),
            (empty.as_path(), 0o555),
            (root.path(), 0o555),
        ] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        references
            .register(root.path(), digest.clone())
            .await
            .unwrap();
        references.register(root.path(), digest).await.unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(empty, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[tokio::test]
    async fn retains_directory_members() {
        use crate::inputs::{Inputs, Mode, Snapshots};
        use crate::runner::Source;

        let root = tempfile::tempdir().unwrap();
        let release = root.path().join("release");
        let index = release.join("index");
        let empty = index.join("empty");
        let files = index.join("files");
        std::fs::create_dir_all(&empty).unwrap();
        std::fs::create_dir(&files).unwrap();
        let file = files.join("tool");
        std::fs::write(&file, b"ACGT").unwrap();
        std::os::unix::fs::symlink("files", index.join("linked")).unwrap();
        for (path, mode) in [
            (file.as_path(), 0o555),
            (files.as_path(), 0o555),
            (empty.as_path(), 0o555),
            (index.as_path(), 0o555),
            (release.as_path(), 0o555),
        ] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        let expected = reference_digest(&index).await.unwrap();
        let mut references = References::default();
        references
            .register(&release, reference_digest(&release).await.unwrap())
            .await
            .unwrap();
        let store = content_store::Store::local(&root.path().join("store")).unwrap();
        let settings = crate::Settings {
            downloads: root.path().join("downloads"),
            import_inputs: true,
            storage: storage::Storage::default(),
            digests: None,
        };
        let inputs = Inputs {
            store: &store,
            settings: &settings,
            references: &references,
            mode: Mode::Run,
            snapshots: Snapshots::new(&settings, "test"),
        };
        let input = workflow_graph::Input {
            path: "index".into(),
            source: index.to_string_lossy().into_owned(),
            directory: true,
            ..Default::default()
        };
        let (siblings, layout) = inputs
            .siblings(&[input.clone(), input.clone()])
            .await
            .unwrap();
        assert!(siblings.is_empty());
        assert!(layout.is_none());
        for _ in 0..2 {
            let (input, source) = inputs.input(&input, &siblings).await.unwrap();
            assert_eq!(input.kind, FileKind::Directory);
            assert_eq!(input.digest, expected);
            assert!(matches!(source, Source::Local(path) if path == index));
        }
        let tree = store.get_tree(&expected).await.unwrap();
        assert!(tree.entries.iter().any(|entry| matches!(entry, TreeEntry::File { path, executable: true, .. } if path == "files/tool")));
        assert!(
            tree.entries.iter().any(
                |entry| matches!(entry, TreeEntry::EmptyDirectory { path } if path == "empty")
            )
        );
        assert!(tree.entries.iter().any(|entry| matches!(entry, TreeEntry::Symlink { path, target } if path == "linked" && target == "files")));
        assert!(!store.has_content(&Digest::sha256(b"ACGT")).await.unwrap());
        assert!(!settings.downloads.join("input-snapshots").exists());
        let (_, digest, kind) = references.find(&empty).await.unwrap().unwrap();
        assert_eq!(kind, FileKind::Directory);
        assert_eq!(digest, Tree::default().digest().unwrap());
        assert!(references.tree(&digest).unwrap().entries.is_empty());
        std::fs::set_permissions(&release, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(release.join("changed"), b"outside selected subtree").unwrap();
        assert!(references.find(&index).await.is_err());
        for path in [&index, &files, &empty] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
}
