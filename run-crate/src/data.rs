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

    /// The files and directories copied so far, and their size.
    pub(crate) fn totals(&self) -> (usize, u64) {
        let copied = self.placed.values().filter_map(|placed| match placed {
            Placed::Copied { size, .. } => Some(*size),
            _ => None,
        });
        copied.fold((0, 0), |(count, total), size| (count + 1, total + size))
    }

    /// Places the file or directory at a location, once per location.
    pub(crate) fn place(&mut self, location: &str) -> Result<Placed, CrateError> {
        if let Some(placed) = self.placed.get(location) {
            return Ok(placed.clone());
        }
        let path = Path::new(location);
        let placed = if location.contains("://") {
            Placed::Remote {
                url: location.to_owned(),
            }
        } else if path.is_dir() {
            self.copy_directory(path)?
        } else if path.is_file() {
            let (id, digest, size) = self.copy_file(&mut File::open(path)?)?;
            Placed::Copied {
                id,
                digest,
                size,
                directory: false,
            }
        } else {
            Placed::Absent
        };
        self.placed.insert(location.to_owned(), placed.clone());
        Ok(placed)
    }

    /// Places a file that the workflow wrote itself, like a CWL `InitialWorkDir` entry.
    pub(crate) fn place_content(&mut self, content: &str) -> Result<Placed, CrateError> {
        let (id, digest, size) = self.copy_file(&mut content.as_bytes())?;
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
    fn copy_file(&mut self, source: &mut dyn Read) -> Result<(String, Digest, u64), CrateError> {
        std::fs::create_dir_all(&self.directory)?;
        let part = self.part();
        let (digest, size) = copy_hashed(source, &part)?;
        let target = self.directory.join(digest.hex());
        if target.exists() {
            std::fs::remove_file(&part)?;
        } else {
            std::fs::rename(&part, &target)?;
        }
        Ok((format!("data/{}", digest.hex()), digest, size))
    }

    /// Copies a directory into `data/<hex>/`, where the digest is that of its manifest, as the
    /// store computes it.
    fn copy_directory(&mut self, source: &Path) -> Result<Placed, CrateError> {
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
                } else if entry.path().is_file() {
                    let target = part.join(&relative);
                    if let Some(parent) = target.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    let (digest, bytes) = copy_hashed(&mut File::open(entry.path())?, &target)?;
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
fn copy_hashed(source: &mut dyn Read, target: &Path) -> Result<(Digest, u64), CrateError> {
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
