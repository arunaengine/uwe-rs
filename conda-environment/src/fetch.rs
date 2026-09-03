//! Streams repodata and verified packages into the preparation cache through caller transport.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest as _, Sha256};

use crate::{Environment, Lock, base, channels, repodata_path};

static NEXT: AtomicU64 = AtomicU64::new(0);

/// Downloads each channel's platform and noarch repodata with caller-supplied transport.
/// A failed fetch or parse leaves the prior cached file intact. The callback must report
/// HTTP status and transport failures as errors, and stream the entire response into `out`.
pub fn fetch_repodata(
    environment: &Environment,
    folder: &Path,
    platform: &str,
    mut fetch: impl FnMut(&str, &mut dyn Write) -> io::Result<()>,
) -> io::Result<()> {
    let channels = channels(&environment.channels);
    if !component(platform)
        || channels
            .iter()
            .any(|channel| !channel.split('/').all(component))
    {
        return Err(io::Error::other(
            "invalid Conda channel or platform cache path",
        ));
    }
    for channel in channels {
        for subdir in [platform, "noarch"] {
            let path = repodata_path(folder, &channel, subdir);
            let url = format!("{}/{subdir}/repodata.json", base(&channel));
            publish(
                &path,
                |out| fetch(&url, out),
                |path| {
                    serde_json::from_reader::<_, crate::repodata::Repodata>(File::open(path)?)
                        .map(|_| ())
                        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
                },
            )?;
        }
    }
    Ok(())
}

/// Downloads the lock's real packages into a SHA-256 addressed cache, in lock order.
/// Cached bytes are verified before reuse. Missing or invalid SHA-256 metadata is refused,
/// including MD5-only locks. This prepares bytes and never executes Conda on the host.
pub fn fetch_packages(
    lock: &Lock,
    folder: &Path,
    mut fetch: impl FnMut(&str, &mut dyn Write) -> io::Result<()>,
) -> io::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for package in &lock.packages {
        let digest = package
            .sha256
            .as_deref()
            .filter(|digest| {
                digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("package {} needs a valid SHA-256 digest", package.name),
                )
            })?
            .to_ascii_lowercase();
        let path = folder.join(&digest);
        if verify(&path, &digest).is_err() {
            publish(
                &path,
                |out| fetch(&package.url, out),
                |path| verify(path, &digest),
            )?;
        }
        paths.push(path);
    }
    Ok(paths)
}

fn component(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
}

fn verify(path: &Path, expected: &str) -> io::Result<()> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    let actual: String = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if actual != expected {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "package SHA-256 mismatch",
        ));
    }
    Ok(())
}

fn publish(
    path: &Path,
    fetch: impl FnOnce(&mut dyn Write) -> io::Result<()>,
    verify: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<()> {
    let folder = path
        .parent()
        .ok_or_else(|| io::Error::other("cache path has no parent"))?;
    std::fs::create_dir_all(folder)?;
    let (temporary, mut file) = loop {
        let temporary = folder.join(format!(
            ".fetch-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => break (temporary, file),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    };
    let result = (|| {
        fetch(&mut file)?;
        file.sync_all()?;
        drop(file);
        verify(&temporary)?;
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}
