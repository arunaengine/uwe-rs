//! The data files of a crate. Each distinct file or directory of the run is copied once into
//! `data/`, named by its SHA-256 digest, as CWLProv and runcrate name them. A copy, not a link,
//! so that nothing done to the crate can change the store.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};
use task_model::{Digest, Tree, TreeEntry};

use crate::CrateError;

/// Where a file of the run is in the crate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Placed {
    /// Copied into the crate: `id` is `data/<hex>` for a file, `data/<hex>/` for a directory.
    Copied {
        id: String,
        digest: Digest,
        size: u64,
        directory: bool,
    },
    /// A file on the web or in a bucket, which stays where it is.
    Remote { url: String },
    /// Not at its location: deleted since the run, or never made, like an input that a
    /// simulated run assumed.
    Absent,
}

pub(crate) struct Data {
    /// The `data` directory of the crate.
    directory: PathBuf,
    placed: HashMap<String, Placed>,
    /// Numbers the parts being copied, so that each has its own name.
    parts: u64,
}

impl Data {
    pub(crate) fn new(crate_root: &Path) -> Self {
        Data {
            directory: crate_root.join("data"),
            placed: HashMap::new(),
            parts: 0,
        }
    }

    /// The distinct files and directories copied so far, and their size.
    pub(crate) fn totals(&self) -> (usize, u64) {
        let copied: std::collections::BTreeMap<&str, u64> = self
            .placed
            .values()
            .filter_map(|placed| match placed {
                Placed::Copied { id, size, .. } => Some((id.as_str(), *size)),
                _ => None,
            })
            .collect();
        (copied.len(), copied.values().sum())
    }

    /// Places the file or directory at a location, once per location.
    pub(crate) fn place(&mut self, location: &str) -> Result<Placed, CrateError> {
        self.place_checked(location, None)
    }

    pub(crate) fn place_checked(
        &mut self,
        location: &str,
        expected: Option<&str>,
    ) -> Result<Placed, CrateError> {
        let key = format!("{}:{location}", expected.unwrap_or_default());
        if let Some(placed) = self.placed.get(&key) {
            return Ok(placed.clone());
        }
        let path = Path::new(location);
        let placed = if location.contains("://") {
            Placed::Remote {
                url: location.to_owned(),
            }
        } else if path.is_dir() {
            self.copy_directory(path, expected)?
        } else if path.is_file() {
            match self.copy_file(&mut File::open(path)?, expected)? {
                Some((id, digest, size)) => Placed::Copied {
                    id,
                    digest,
                    size,
                    directory: false,
                },
                None => Placed::Absent,
            }
        } else {
            Placed::Absent
        };
        self.placed.insert(key, placed.clone());
        Ok(placed)
    }

    /// Places a file that the workflow wrote itself, like a CWL `InitialWorkDir` entry.
    pub(crate) fn place_content(&mut self, content: &str) -> Result<Placed, CrateError> {
        let (id, digest, size) = self
            .copy_file(&mut content.as_bytes(), None)?
            .expect("no digest constraint");
        let key = format!("content:{digest}");
        let placed = Placed::Copied {
            id,
            digest,
            size,
            directory: false,
        };
        self.placed.insert(key, placed.clone());
        Ok(placed)
    }

    /// Copies bytes into `data/<hex>` and hashes them on the way, so that each file is read
    /// once. The id is relative to the crate root.
    fn copy_file(
        &mut self,
        source: &mut dyn Read,
        expected: Option<&str>,
    ) -> Result<Option<(String, Digest, u64)>, CrateError> {
        std::fs::create_dir_all(&self.directory)?;
        let part = self.part();
        let (digest, size) = copy_hashed(source, &part)?;
        if expected.is_some_and(|expected| digest.to_string() != expected) {
            std::fs::remove_file(&part)?;
            return Ok(None);
        }
        let target = self.directory.join(digest.hex());
        if target.exists() {
            std::fs::remove_file(&part)?;
        } else {
            std::fs::rename(&part, &target)?;
        }
        Ok(Some((format!("data/{}", digest.hex()), digest, size)))
    }

    /// Copies a directory into `data/<hex>/`, where the digest is that of its manifest, as the
    /// store computes it.
    fn copy_directory(
        &mut self,
        source: &Path,
        expected: Option<&str>,
    ) -> Result<Placed, CrateError> {
        let part = self.part();
        let mut entries = Vec::new();
        let mut size = 0;
        let mut pending = vec![(source.to_owned(), String::new())];
        while let Some((directory, prefix)) = pending.pop() {
            let mut empty = true;
            for entry in std::fs::read_dir(&directory)? {
                let entry = entry?;
                empty = false;
                let name = entry.file_name().to_string_lossy().into_owned();
                let relative = if prefix.is_empty() {
                    name
                } else {
                    format!("{prefix}/{name}")
                };
                if entry.file_type()?.is_dir() {
                    pending.push((entry.path(), relative));
                } else if entry.file_type()?.is_symlink() && !entry.path().is_file() {
                    let link = std::fs::read_link(entry.path())?;
                    let mut depth = Path::new(&relative)
                        .parent()
                        .unwrap_or(Path::new(""))
                        .components()
                        .count();
                    let invalid = link.is_absolute()
                        || link.components().any(|component| match component {
                            std::path::Component::ParentDir => match depth.checked_sub(1) {
                                Some(next) => {
                                    depth = next;
                                    false
                                }
                                None => true,
                            },
                            std::path::Component::Normal(_) => {
                                depth += 1;
                                false
                            }
                            _ => false,
                        });
                    let target = link.to_str().filter(|_| !invalid).ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            "unsupported directory link in historical export",
                        )
                    })?;
                    let copy = part.join(&relative);
                    std::fs::create_dir_all(
                        copy.parent().expect("a directory entry has a parent"),
                    )?;
                    std::os::unix::fs::symlink(&link, &copy)?;
                    entries.push(TreeEntry::Symlink {
                        path: relative,
                        target: target.to_owned(),
                    });
                } else if entry.path().is_file() {
                    let target = part.join(&relative);
                    if let Some(parent) = target.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    let (digest, bytes) = copy_hashed(&mut File::open(entry.path())?, &target)?;
                    std::fs::set_permissions(
                        &target,
                        std::fs::metadata(entry.path())?.permissions(),
                    )?;
                    size += bytes;
                    entries.push(TreeEntry::File {
                        path: relative,
                        digest,
                        size: bytes,
                        executable: executable(&entry.path()),
                    });
                }
            }
            if empty && !prefix.is_empty() {
                std::fs::create_dir_all(part.join(&prefix))?;
                entries.push(TreeEntry::EmptyDirectory { path: prefix });
            }
        }
        std::fs::create_dir_all(&part)?;
        let digest = Tree { entries }.digest().map_err(CrateError::Tree)?;
        if expected.is_some_and(|expected| digest.to_string() != expected) {
            std::fs::remove_dir_all(&part)?;
            return Ok(Placed::Absent);
        }
        let target = self.directory.join(digest.hex());
        if target.exists() {
            std::fs::remove_dir_all(&part)?;
        } else {
            std::fs::rename(&part, &target)?;
        }
        Ok(Placed::Copied {
            id: format!("data/{}/", digest.hex()),
            digest,
            size,
            directory: true,
        })
    }

    /// A new temporary name inside `data/`.
    fn part(&mut self) -> PathBuf {
        self.parts += 1;
        self.directory.join(format!(".part-{}", self.parts))
    }
}

/// Copies bytes to a new file and returns their digest and size.
pub(crate) fn copy_hashed(
    source: &mut dyn Read,
    target: &Path,
) -> Result<(Digest, u64), CrateError> {
    let mut output = File::create(target)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1 << 20];
    let mut size = 0;
    loop {
        let read = source.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        output.write_all(&buffer[..read])?;
        size += read as u64;
    }
    Ok((Digest::from_sha256(&hasher.finalize()), size))
}

fn executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_links_keep_their_spelling_and_tree_digest() {
        let root = tempfile::tempdir().expect("a directory");
        let source = root.path().join("input");
        std::fs::create_dir_all(source.join("nested")).expect("creates tree");
        std::os::unix::fs::symlink("..", source.join("nested/back")).expect("makes cyclic link");
        let expected = Tree {
            entries: vec![TreeEntry::Symlink {
                path: "nested/back".into(),
                target: "..".into(),
            }],
        }
        .digest()
        .expect("hashes tree");
        let mut data = Data::new(&root.path().join("crate"));
        let Placed::Copied { id, digest, .. } = data
            .place_checked(&source.to_string_lossy(), Some(&expected.to_string()))
            .expect("copies link tree")
        else {
            panic!("historical link tree was copied");
        };
        assert_eq!(digest, expected);
        let copy = root.path().join("crate").join(id);
        assert_eq!(
            std::fs::read_link(copy.join("nested/back")).expect("reads link"),
            Path::new("..")
        );
        let mut archived = Data::new(&root.path().join("again"));
        assert!(matches!(
            archived
                .place_checked(&copy.to_string_lossy(), Some(&expected.to_string()))
                .expect("checks archived view"),
            Placed::Copied { .. }
        ));
    }

    #[test]
    fn recorded_digest_rejects_replacements_without_publishing_them() {
        let root = tempfile::tempdir().expect("a directory");
        let source = root.path().join("input");
        std::fs::create_dir_all(&source).expect("creates input tree");
        let file = source.join("a.txt");
        std::fs::write(&file, "original").expect("writes original");
        let mut data = Data::new(&root.path().join("crate"));
        let file_digest = Digest::sha256(b"original").to_string();
        let Placed::Copied {
            digest: tree_digest,
            ..
        } = data.place(&source.to_string_lossy()).expect("places tree")
        else {
            panic!("tree was copied");
        };
        std::fs::write(&file, "replacement").expect("changes input");
        assert_eq!(
            data.place_checked(&file.to_string_lossy(), Some(&file_digest))
                .expect("checks file"),
            Placed::Absent
        );
        assert_eq!(
            data.place_checked(&source.to_string_lossy(), Some(&tree_digest.to_string()))
                .expect("checks tree"),
            Placed::Absent
        );
        assert!(
            !data
                .directory
                .join(Digest::sha256(b"replacement").hex())
                .exists()
        );
        assert!(
            std::fs::read_dir(&data.directory)
                .expect("lists")
                .all(|entry| !entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".part"))
        );
        std::fs::write(&file, "original").expect("restores input");
        let mut fresh = Data::new(&root.path().join("fresh-crate"));
        assert!(matches!(
            fresh
                .place_checked(&file.to_string_lossy(), Some(&file_digest))
                .expect("checks historical bytes"),
            Placed::Copied { .. }
        ));
    }

    #[test]
    fn files_are_copied_once_by_digest() {
        let root = tempfile::tempdir().expect("a temporary directory");
        let source = root.path().join("source");
        std::fs::create_dir_all(source.join("inner/empty")).expect("makes folders");
        std::fs::write(source.join("inner/a.txt"), "a").expect("writes");
        std::fs::write(root.path().join("same.txt"), "a").expect("writes");
        let crate_root = root.path().join("crate");
        let mut data = Data::new(&crate_root);
        let a = root.path().join("same.txt");
        let Placed::Copied { id, digest, .. } = data.place(&a.to_string_lossy()).expect("places")
        else {
            panic!("the file is copied");
        };
        assert_eq!(digest, Digest::sha256(b"a"));
        assert_eq!(id, format!("data/{}", digest.hex()));
        assert_eq!(std::fs::read(crate_root.join(&id)).expect("reads"), b"a");
        let Placed::Copied { id, size, .. } =
            data.place(&source.to_string_lossy()).expect("places")
        else {
            panic!("the directory is copied");
        };
        assert!(id.ends_with('/'));
        assert_eq!(size, 1);
        assert!(crate_root.join(&id).join("inner/empty").is_dir());
        assert_eq!(data.place("/no/such/file").expect("places"), Placed::Absent);
        let url = "https://example.org/reads.fq";
        assert_eq!(
            data.place(url).expect("places"),
            Placed::Remote { url: url.into() }
        );
        // The same content at a second place is copied once.
        let copy = root.path().join("copy.txt");
        std::fs::write(&copy, "a").expect("writes");
        data.place(&copy.to_string_lossy()).expect("places");
        assert_eq!(data.totals(), (2, 2));
        let names: Vec<String> = std::fs::read_dir(crate_root.join("data"))
            .expect("lists")
            .map(|entry| {
                entry
                    .expect("an entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert!(
            names.iter().all(|name| !name.starts_with(".part")),
            "{names:?}"
        );
    }
}
