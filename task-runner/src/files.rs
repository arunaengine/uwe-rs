//! The files that the tasks of a run made, for deliveries and drivers. A location is a path on
//! this machine or an address of a storage provider, like an output in a store on S3, and a
//! target is a path or an address, like a `publishDir` in a bucket. A link needs both on this
//! machine; else the file is copied.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io;
use std::path::{Path, PathBuf};

use storage::{Blocking, Storage, StorageError, address};
use workflow_graph::{Delivery, DeliveryMode, Files, LocalFiles};

/// The files of a run, wherever its store and its targets are.
pub struct StorageFiles {
    storage: Blocking,
    /// Where copies of remote files go.
    downloads: PathBuf,
}

impl StorageFiles {
    pub fn new(storage: &Storage, downloads: PathBuf) -> StorageFiles {
        StorageFiles {
            storage: storage.blocking(),
            downloads,
        }
    }

    /// A copy on this machine of the file or the directory at an address.
    fn copy_here(&self, location: &str) -> io::Result<PathBuf> {
        match self.storage.stat(location).map_err(other)? {
            Some(found) if found.directory => {
                let folder = self
                    .downloads
                    .join(self.storage.download_name(location).map_err(other)?)
                    .join("content");
                self.download_directory(location, &folder)?;
                Ok(folder)
            }
            _ => self.storage.fetch(location, &self.downloads).map_err(other),
        }
    }

    /// Downloads each file below an address into a folder.
    fn download_directory(&self, location: &str, folder: &Path) -> io::Result<()> {
        std::fs::create_dir_all(folder)?;
        for entry in self.storage.list(location).map_err(other)? {
            let inside = format!("{}/{}", location.trim_end_matches('/'), entry.name);
            let local = folder.join(&entry.name);
            match entry.directory {
                true => self.download_directory(&inside, &local)?,
                false => self.storage.download(&inside, &local).map_err(other)?,
            }
        }
        Ok(())
    }

    /// Uploads a local file, or each file of a local directory below the address.
    fn upload(&self, local: &Path, target: &str) -> io::Result<()> {
        if !local.is_dir() {
            return self.storage.upload(local, target).map_err(other);
        }
        for entry in std::fs::read_dir(local)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let inside = format!("{}/{name}", target.trim_end_matches('/'));
            self.upload(&entry.path(), &inside)?;
        }
        Ok(())
    }
}

impl Files for StorageFiles {
    fn local(&self, location: &str) -> io::Result<PathBuf> {
        if let Some(path) = location.strip_prefix("file://") {
            return Ok(PathBuf::from(address::decode(path)));
        }
        match address::scheme(location) {
            None => Ok(PathBuf::from(location)),
            Some(_) => self.copy_here(location),
        }
    }

    fn deliver(&self, delivery: &Delivery) -> io::Result<()> {
        let remote = |text: &str| address::scheme(text).is_some();
        if !remote(&delivery.location) && !remote(&delivery.target) {
            return LocalFiles.deliver(delivery);
        }
        let local = self.local(&delivery.location)?;
        if remote(&delivery.target) {
            return self.upload(&local, &delivery.target);
        }
        // A link to the copy of a remote file would break when the copy goes.
        LocalFiles.deliver(&Delivery {
            location: local.to_string_lossy().into_owned(),
            target: delivery.target.clone(),
            mode: DeliveryMode::Copy,
        })
    }
}

fn other(error: StorageError) -> io::Error {
    io::Error::other(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_reads_preserve() {
        use std::os::unix::fs::MetadataExt;
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("tree");
        std::fs::create_dir(&directory).unwrap();
        let member = directory.join("data");
        std::fs::write(&member, "original").unwrap();
        let before = std::fs::metadata(&member).unwrap();
        let files = StorageFiles::new(&Storage::default(), root.path().join("downloads"));
        let found = files
            .local(&format!("file://{}", directory.display()))
            .unwrap();
        let after = std::fs::metadata(&member).unwrap();
        assert_eq!(found, directory);
        assert_eq!(before.ino(), after.ino());
        assert_eq!(before.modified().unwrap(), after.modified().unwrap());
        assert_eq!(std::fs::read_to_string(member).unwrap(), "original");
        assert!(!root.path().join("downloads").exists());
    }

    /// Storage whose `s3://results/` and `s3://store/` are folders below `root`.
    fn folders(root: &Path) -> Storage {
        let text = format!(
            "[storage.results]\ntype = \"folder\"\nprefixes = [\"s3://results/\"]\npath = \"{0}/results\"\n\
             [storage.store]\ntype = \"folder\"\nprefixes = [\"s3://store/\"]\npath = \"{0}/store\"\n",
            root.display()
        );
        let config = config_file::ConfigFile::parse(&text, Path::new("config.toml")).expect("TOML");
        Storage::from_config(&config).expect("valid settings")
    }

    #[test]
    fn delivers_remote_addresses() {
        let root = tempfile::tempdir().expect("a temporary directory");
        let files = StorageFiles::new(&folders(root.path()), root.path().join("downloads"));
        let made = root.path().join("made");
        std::fs::create_dir_all(made.join("db/sub")).unwrap();
        std::fs::write(made.join("out.txt"), "out\n").unwrap();
        std::fs::write(made.join("db/sub/a.txt"), "a\n").unwrap();
        let deliver = |location: String, target: &str, mode| {
            let delivery = Delivery {
                location,
                target: target.to_owned(),
                mode,
            };
            files.deliver(&delivery).expect("delivers");
        };
        let local = |name: &str| made.join(name).to_string_lossy().into_owned();
        // From this machine to an address: a file, and a directory with all it holds.
        deliver(
            local("out.txt"),
            "s3://results/run/out.txt",
            DeliveryMode::Link,
        );
        deliver(local("db"), "s3://results/run/db", DeliveryMode::Copy);
        let results = root.path().join("results/run");
        assert_eq!(
            std::fs::read_to_string(results.join("out.txt")).unwrap(),
            "out\n"
        );
        assert_eq!(
            std::fs::read_to_string(results.join("db/sub/a.txt")).unwrap(),
            "a\n"
        );
        // From an address, like a store on S3, to this machine: links become copies.
        let stored = root.path().join("store/runs/1");
        std::fs::create_dir_all(stored.join("tree")).unwrap();
        std::fs::write(stored.join("x.txt"), "x\n").unwrap();
        std::fs::write(stored.join("tree/y.txt"), "y\n").unwrap();
        let target = root.path().join("here/x.txt");
        let target = target.to_string_lossy();
        deliver(
            "s3://store/runs/1/x.txt".into(),
            &target,
            DeliveryMode::Link,
        );
        let copied = std::fs::symlink_metadata(target.as_ref()).unwrap();
        assert!(copied.is_file());
        let tree = root.path().join("here/tree");
        deliver(
            "s3://store/runs/1/tree".into(),
            &tree.to_string_lossy(),
            DeliveryMode::Copy,
        );
        assert_eq!(std::fs::read_to_string(tree.join("y.txt")).unwrap(), "y\n");
        // From an address to another address.
        deliver(
            "s3://store/runs/1/x.txt".into(),
            "s3://results/x.txt",
            DeliveryMode::Copy,
        );
        let delivered = root.path().join("results/x.txt");
        assert_eq!(std::fs::read_to_string(delivered).unwrap(), "x\n");
        // Drivers read a remote location through a local copy.
        let copy = files.local("s3://store/runs/1/x.txt").unwrap();
        assert_eq!(std::fs::read_to_string(copy).unwrap(), "x\n");
    }
}
