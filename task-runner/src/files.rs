//! Reads and delivers task outputs from local paths and remote storage addresses.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use storage::{Blocking, Storage, StorageError, address};
use workflow_graph::{Delivery, DeliveryMode, Files, LocalFiles};

/// The files of a run, wherever its store and its targets are.
pub struct StorageFiles {
    storage: Blocking,
    /// Where copies of remote files go.
    downloads: PathBuf,
    /// The copy of each directory that this process made, by its download name.
    copied: Mutex<HashMap<String, Arc<Mutex<Option<PathBuf>>>>>,
}

impl StorageFiles {
    pub fn new(storage: &Storage, downloads: PathBuf) -> StorageFiles {
        StorageFiles {
            storage: storage.blocking(),
            downloads,
            copied: Mutex::default(),
        }
    }

    /// A copy on this machine of the file or the directory at an address.
    fn copy_here(&self, location: &str) -> io::Result<PathBuf> {
        match self.storage.stat(location).map_err(other)? {
            Some(found) if found.directory => {
                // As fetch does, so that `iunctus clean` leaves the copy alone while it grows.
                self.storage.hold(&self.downloads).map_err(other)?;
                let name = self.storage.download_name(location).map_err(other)?;
                let turn = (self
                    .copied
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner()))
                .entry(name.clone())
                .or_default()
                .clone();
                let mut done = turn.lock().unwrap_or_else(|poison| poison.into_inner());
                if let Some(bundle) = &*done {
                    return Ok(bundle.join("content"));
                }
                let bundle = self.copy_directory(location, &name)?;
                *done = Some(bundle.clone());
                Ok(bundle.join("content"))
            }
            _ => self.storage.fetch(location, &self.downloads).map_err(other),
        }
    }

    /// Downloads a directory whole into a temporary folder and only then renames it to a
    /// bundle of this process, as tasks of other runs may read their own copy while it changes.
    /// A crash leaves a `.tmp` folder; `iunctus clean` removes it and old bundles.
    fn copy_directory(&self, location: &str, name: &str) -> io::Result<PathBuf> {
        std::fs::create_dir_all(&self.downloads)?;
        let ready = tempfile::tempdir_in(&self.downloads)?;
        self.download_directory(location, &ready.path().join("content"))?;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let bundle = (self.downloads).join(format!("{name}-{}-{stamp}", std::process::id()));
        std::fs::rename(ready.path(), &bundle)?;
        Ok(bundle)
    }

    /// Downloads each file below an address into a folder.
    fn download_directory(&self, location: &str, folder: &Path) -> io::Result<()> {
        std::fs::create_dir_all(folder)?;
        for entry in self.storage.list(location).map_err(other)? {
            let mut components = Path::new(&entry.name).components();
            if !matches!(
                (components.next(), components.next()),
                (Some(std::path::Component::Normal(_)), None)
            ) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "storage listed an invalid entry name",
                ));
            }
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
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};

    fn ftp_session(mut stream: TcpStream) {
        let mut reader = BufReader::new(stream.try_clone().expect("a stream"));
        writeln!(stream, "220 fixture\r").unwrap();
        let mut data = None;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
            let line = line.trim_end();
            let (command, argument) = line.split_once(' ').unwrap_or((line, ""));
            let answer = match command {
                "USER" => "331 password please".to_owned(),
                "PASS" => "230 welcome".to_owned(),
                "TYPE" => "200 type set".to_owned(),
                "SIZE" => "550 folder".to_owned(),
                "CWD" => "250 changed".to_owned(),
                "PASV" => {
                    let listener = TcpListener::bind("127.0.0.1:0").expect("a data port");
                    let port = listener.local_addr().expect("an address").port();
                    data = Some(listener);
                    format!("227 passive (127,0,0,1,{},{})", port / 256, port % 256)
                }
                "RETR" | "LIST" => match data.take() {
                    Some(listener) => {
                        writeln!(stream, "150 sending\r").unwrap();
                        let (mut transfer, _) = listener.accept().expect("a transfer");
                        let body: &[u8] = match (command, argument) {
                            ("LIST", "/bad") => b"-rw-r--r-- 1 owner group 8 Jan 01 00:00 ../../../download-sentinel\r\n",
                            ("LIST", _) => b"-rw-r--r-- 1 owner group 4 Jan 01 00:00 safe.txt\r\n",
                            _ => b"safe",
                        };
                        transfer.write_all(body).unwrap();
                        drop(transfer);
                        "226 sent".to_owned()
                    }
                    None => "550 no transfer".to_owned(),
                },
                "QUIT" => {
                    writeln!(stream, "221 bye\r").unwrap();
                    return;
                }
                _ => "502 unknown".to_owned(),
            };
            writeln!(stream, "{answer}\r").unwrap();
        }
    }

    fn start_ftp() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
        let address = format!("ftp://{}", listener.local_addr().expect("an address"));
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                std::thread::spawn(move || ftp_session(stream));
            }
        });
        address
    }

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

    #[test]
    fn rejects_list_escape() {
        let root = tempfile::tempdir().expect("a temporary directory");
        let downloads = root.path().join("downloads");
        let storage = Storage::default();
        let files = StorageFiles::new(&storage, downloads.clone());
        let server = start_ftp();
        let safe = format!("{server}/safe");
        let copy = files.local(&safe).expect("a safe directory");
        assert!(copy.starts_with(&downloads));
        assert_eq!(
            std::fs::read_to_string(copy.join("safe.txt")).unwrap(),
            "safe"
        );

        let sentinel = root.path().join("download-sentinel");
        std::fs::write(&sentinel, "original").unwrap();
        let error = files
            .local(&format!("{server}/bad"))
            .expect_err("an invalid listing");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(std::fs::read_to_string(sentinel).unwrap(), "original");
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
    fn directory_copies_hold() {
        let root = tempfile::tempdir().expect("a temporary directory");
        let downloads = root.path().join("downloads");
        std::fs::create_dir_all(&downloads).unwrap();
        let stored = root.path().join("store/runs/1/tree");
        std::fs::create_dir_all(&stored).unwrap();
        std::fs::write(stored.join("y.txt"), "y\n").unwrap();
        let files = StorageFiles::new(&folders(root.path()), downloads.clone());
        // Clean-up holds the downloads alone, so the copy waits.
        let alone = storage::hold_downloads(&downloads).unwrap();
        assert!(alone.is_some());
        let (sender, copied) = std::sync::mpsc::channel();
        let copying = std::thread::spawn(move || {
            let copy = files.local("s3://store/runs/1/tree").expect("a copy");
            sender.send(copy).unwrap();
            files
        });
        let waiting = copied.recv_timeout(std::time::Duration::from_millis(300));
        assert!(waiting.is_err(), "the copy waits for clean-up");
        drop(alone);
        let copy = copied.recv().unwrap();
        assert_eq!(std::fs::read_to_string(copy.join("y.txt")).unwrap(), "y\n");
        // While the run lives, clean-up leaves its downloads alone.
        let files = copying.join().unwrap();
        assert!(storage::hold_downloads(&downloads).unwrap().is_none());
        drop(files);
    }

    #[test]
    fn partial_copies_vanish() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().expect("a temporary directory");
        let downloads = root.path().join("downloads");
        let stored = root.path().join("store/runs/2/tree");
        std::fs::create_dir_all(&stored).unwrap();
        std::fs::write(stored.join("a.txt"), "a\n").unwrap();
        let broken = stored.join("b.txt");
        std::fs::write(&broken, "b\n").unwrap();
        let location = "s3://store/runs/2/tree";
        let storage = folders(root.path());
        let bundle = downloads.join(storage.blocking().download_name(location).unwrap());
        // A source that fails after some files, as a crash partway does.
        std::fs::set_permissions(&broken, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::File::open(&broken).is_ok() {
            return; // Root reads the file anyway.
        }
        let files = StorageFiles::new(&storage, downloads.clone());
        assert!(files.local(location).is_err());
        assert!(!bundle.exists(), "no partial copy under the download name");
        // An older version left a partial copy with a stale file there.
        std::fs::create_dir_all(bundle.join("content")).unwrap();
        std::fs::write(bundle.join("content/a.txt"), "partial").unwrap();
        std::fs::write(bundle.join("content/old.txt"), "old").unwrap();
        std::fs::set_permissions(&broken, std::fs::Permissions::from_mode(0o644)).unwrap();
        let files = StorageFiles::new(&storage, downloads.clone());
        let copy = files.local(location).expect("a copy");
        assert_ne!(copy, bundle.join("content"), "the next run downloads again");
        assert_eq!(std::fs::read_to_string(copy.join("a.txt")).unwrap(), "a\n");
        assert_eq!(std::fs::read_to_string(copy.join("b.txt")).unwrap(), "b\n");
        assert!(!copy.join("old.txt").exists());
        let left = (std::fs::read_dir(&downloads).unwrap())
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".tmp"))
            .count();
        assert_eq!(left, 0, "no partial copy stays");
    }

    #[test]
    fn copies_stay_readable() {
        let root = tempfile::tempdir().expect("a temporary directory");
        let downloads = root.path().join("downloads");
        let stored = root.path().join("store/runs/3/tree");
        std::fs::create_dir_all(&stored).unwrap();
        std::fs::write(stored.join("a.txt"), "first\n").unwrap();
        let location = "s3://store/runs/3/tree";
        // Two runs at once: the second copies a newer version while the first reads its own.
        let first = StorageFiles::new(&folders(root.path()), downloads.clone());
        let read = first.local(location).expect("a copy");
        std::fs::write(stored.join("a.txt"), "second\n").unwrap();
        let second = StorageFiles::new(&folders(root.path()), downloads.clone());
        let newer = second.local(location).expect("a copy");
        assert_eq!(
            std::fs::read_to_string(read.join("a.txt")).unwrap(),
            "first\n"
        );
        assert_eq!(
            std::fs::read_to_string(newer.join("a.txt")).unwrap(),
            "second\n"
        );
        assert_eq!(first.local(location).unwrap(), read, "one copy per run");
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
