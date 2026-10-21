//! Packs output artifacts and unpacks and sets the mode of input artifacts as Argo does.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fs::File;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use argo_parser::model::Artifact;
use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use workflow_graph::{Delivery, DeliveryMode, Input};
use zip::{ZipWriter, write::SimpleFileOptions};

use crate::node::{ArtifactValue, Location};
use crate::run::{Failure, Result, Run, scalar_text};

enum Strategy {
    None,
    Tar(Compression),
    Zip,
}

/// What Argo's executor does with a loaded input artifact.
#[derive(Clone, Copy, PartialEq)]
enum Unpack {
    None,
    /// Unpacks it when it is a gzipped tar, the default.
    Detect,
    Tar,
    Zip,
}

#[cfg(test)]
#[path = "archives_tests.rs"]
mod tests;

fn unpacking(artifact: &Artifact) -> Unpack {
    match &artifact.archive {
        Some(node) if node.get("none").is_some() => Unpack::None,
        Some(node) if node.get("zip").is_some() => Unpack::Zip,
        Some(node) if node.get("tar").is_some() => Unpack::Tar,
        _ => Unpack::Detect,
    }
}

fn strategy(artifact: &Artifact) -> Result<Strategy> {
    let Some(node) = &artifact.archive else {
        return Ok(if artifact.s3.is_some() {
            Strategy::Tar(Compression::default())
        } else {
            Strategy::None
        });
    };
    let count = ["none", "tar", "zip"]
        .into_iter()
        .filter(|key| node.get(key).is_some())
        .count();
    if count > 1 {
        return Err(Failure::error("artifact archive must have one strategy"));
    }
    if node.get("none").is_some() {
        return Ok(Strategy::None);
    }
    if node.get("zip").is_some() {
        return Ok(Strategy::Zip);
    }
    let level = node
        .get("tar")
        .and_then(|tar| tar.get("compressionLevel"))
        .map(scalar_text)
        .map(|value| {
            value
                .parse::<i32>()
                .map_err(|_| Failure::error("archive compressionLevel must be an integer"))
        })
        .transpose()?
        .unwrap_or(-1);
    let level = match level {
        -1 => Compression::default(),
        0..=9 => Compression::new(level as u32),
        -2 => {
            return Err(Failure::unsupported(
                "archive compressionLevel -2 requires Huffman-only gzip, which uwe does not support",
            ));
        }
        _ => {
            return Err(Failure::error(
                "archive compressionLevel must be between -2 and 9",
            ));
        }
    };
    Ok(Strategy::Tar(level))
}

pub(crate) fn check(artifact: &Artifact) -> Result<()> {
    let plain = Artifact {
        archive: None,
        ..artifact.clone()
    };
    crate::inputs::check_artifact(&plain)?;
    // A key without a bucket names the artifact repository; a bucket needs a key.
    if let Some(s3) = &artifact.s3
        && s3
            .get("key")
            .map(scalar_text)
            .is_none_or(|key| key.is_empty())
    {
        return Err(Failure::unsupported(format!(
            "artifact {}: an S3 bucket without a key needs the key format of an artifact repository, which uwe does not have; supply a key",
            artifact.name
        )));
    }
    if artifact
        .archive
        .as_ref()
        .and_then(|node| node.get("tar"))
        .and_then(|node| node.get("compressionLevel"))
        .is_some_and(|node| crate::tags::has_tag(&scalar_text(node)))
    {
        return Ok(());
    }
    strategy(artifact).map(|_| ())
}

fn write_zip(writer: &mut ZipWriter<File>, source: &Path, name: &Path) -> io::Result<()> {
    let metadata = std::fs::symlink_metadata(source)?;
    let options = SimpleFileOptions::default().unix_permissions(metadata.permissions().mode());
    let name_text = name
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "zip paths must be UTF-8"))?;
    if metadata.is_symlink() {
        writer.add_symlink(
            name_text,
            std::fs::read_link(source)?.to_string_lossy(),
            options,
        )?;
    } else if metadata.is_dir() {
        writer.add_directory(format!("{name_text}/"), options)?;
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            write_zip(writer, &entry.path(), &name.join(entry.file_name()))?;
        }
    } else if metadata.is_file() {
        writer.start_file(name_text, options)?;
        io::copy(&mut File::open(source)?, writer)?;
    } else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "archive output is not a file, directory or symlink",
        ));
    }
    Ok(())
}

fn pack(source: &Path, target: &Path, name: &Path, strategy: Strategy) -> io::Result<()> {
    let file = File::create(target)?;
    match strategy {
        Strategy::Tar(level) => {
            let mut archive = tar::Builder::new(GzEncoder::new(file, level));
            archive.follow_symlinks(false);
            if std::fs::symlink_metadata(source)?.is_dir() {
                archive.append_dir_all(name, source)?;
            } else {
                archive.append_path_with_name(source, name)?;
            }
            archive.into_inner()?.finish()?;
        }
        Strategy::Zip => {
            let mut archive = ZipWriter::new(file);
            write_zip(&mut archive, source, name)?;
            archive.finish()?;
        }
        Strategy::None => unreachable!(),
    }
    Ok(())
}

impl Run {
    /// The input of a task for an artifact: unpacked, and with the mode it declares, as
    /// Argo's executor loads it. Remote archives are read here in a real run.
    pub(crate) fn input_artifact(
        &mut self,
        value: &ArtifactValue,
        declared: Option<&Artifact>,
    ) -> Result<Option<Input>> {
        if let Some(Location::Url(url)) = &value.location
            && let Some(key) = url.strip_prefix(crate::inputs::REPOSITORY)
            && !self.options.assume_inputs
        {
            if value.optional {
                return Ok(None);
            }
            return Err(Failure::error(format!(
                "artifact {}: no step of this run wrote key {key} of the artifact repository, which uwe keeps inside the run",
                value.name
            )));
        }
        let Some(mut input) = crate::pod::input_of(value) else {
            return Ok(None);
        };
        if let Some(Location::Url(url)) = &value.location
            && let Some(source) = self.git_sources.get(url).cloned()
            && !self.options.assume_inputs
        {
            let failed =
                |message: String| Failure::error(format!("artifact {}: {message}", value.name));
            let client = (self.options.git.clone()).ok_or_else(|| {
                Failure::unsupported("git artifacts need a Git client, which this run has not")
            })?;
            let folder = self
                .private_folder()
                .map_err(|error| failed(error.to_string()))?
                .join("content");
            (client.0)(&source, &folder).map_err(failed)?;
            input.source = folder.to_string_lossy().into_owned();
            input.directory = true;
            return Ok(Some(input));
        }
        let Some(declared) = declared else {
            return Ok(Some(input));
        };
        let unpack = unpacking(declared);
        let local = match &value.location {
            Some(Location::Url(url)) if unpack != Unpack::None => self.download(url),
            Some(Location::File(file)) if unpack != Unpack::None => {
                let file = file.to_string_lossy();
                let raw = self.raw_outputs.contains(Path::new(&*file));
                raw.then(|| self.download(&file)).flatten()
            }
            _ => None,
        };
        let failed = |error: io::Error| {
            Failure::error(format!(
                "artifact {}: cannot unpack: {error}",
                declared.name
            ))
        };
        let mut private = false;
        if let Some(local) = local {
            let archive = match unpack {
                Unpack::Detect => (local.is_file() && is_tarball(&local)).then_some(false),
                Unpack::Tar => Some(false),
                Unpack::Zip => Some(true),
                Unpack::None => None,
            };
            let source = match archive {
                Some(zip) => {
                    let folder = self.private_folder().map_err(failed)?;
                    private = true;
                    unpacked(&local, &folder, zip).map_err(failed)?
                }
                // A directory arrives as a whole, which a remote address cannot.
                None if local.is_dir() => local,
                None => return self.with_mode(input, declared, false),
            };
            input.directory = source.is_dir();
            input.source = source.to_string_lossy().into_owned();
        }
        self.with_mode(input, declared, private)
    }

    /// A local copy of a location, if the run reads files, or none to leave it to the task.
    fn download(&self, location: &str) -> Option<PathBuf> {
        if self.options.assume_inputs {
            return None;
        }
        match &self.files {
            Some(files) => files.local(location).ok(),
            None => Some(PathBuf::from(location)).filter(|path| path.exists()),
        }
    }

    /// Applies `mode` and `recurseMode`: tasks get their own copies, with the mode 0644 or
    /// 0755, so only whether files are executable carries over.
    fn with_mode(
        &mut self,
        mut input: Input,
        declared: &Artifact,
        private: bool,
    ) -> Result<Option<Input>> {
        let Some(node) = &declared.mode else {
            return Ok(Some(input));
        };
        let text = scalar_text(node);
        let mode = parse_mode(&text).ok_or_else(|| {
            Failure::error(format!("artifact {}: invalid mode {text}", declared.name))
        })?;
        if mode & 0o600 != 0o600 {
            self.warn(format!(
                "artifact {}: mode {mode:o} leaves out reading or writing, but tasks get their own writable copy",
                declared.name
            ));
        }
        let recurse = declared
            .recurse_mode
            .as_ref()
            .is_some_and(|node| matches!(node.value, argo_parser::Value::Bool(true)));
        if !input.directory {
            input.executable = mode & 0o111 != 0;
        } else if recurse && Path::new(&input.source).is_dir() {
            let failed = |error: io::Error| {
                Failure::error(format!(
                    "artifact {}: cannot set mode: {error}",
                    declared.name
                ))
            };
            let mut folder = PathBuf::from(&input.source);
            if !private {
                let copy = self.private_folder().map_err(failed)?.join("content");
                copy_tree(&folder, &copy).map_err(failed)?;
                folder = copy;
            }
            set_modes(&folder, mode).map_err(failed)?;
            input.source = folder.to_string_lossy().into_owned();
        }
        Ok(Some(input))
    }

    pub fn archive_output(
        &mut self,
        index: usize,
        artifact: &Artifact,
        location: &str,
    ) -> Result<String> {
        let strategy = strategy(artifact)?;
        if matches!(strategy, Strategy::None) {
            return Ok(location.into());
        }
        if self.archives.is_none() {
            self.archives = Some(tempfile::tempdir().map_err(|error| {
                Failure::error(format!("cannot create artifact archive folder: {error}"))
            })?);
        }
        let extension = if matches!(strategy, Strategy::Zip) {
            "zip"
        } else {
            "tgz"
        };
        let target = self
            .archives
            .as_ref()
            .unwrap()
            .path()
            .join(format!("{index}-{}.{extension}", artifact.name));
        let source = match &self.files {
            Some(files) => files.local(location).map_err(|error| {
                Failure::error(format!("cannot read artifact {}: {error}", artifact.name))
            })?,
            None => PathBuf::from(location),
        };
        let name = Path::new(artifact.path.trim_end_matches('/'))
            .file_name()
            .filter(|name| !name.is_empty())
            .unwrap_or(artifact.name.as_ref());
        pack(&source, &target, Path::new(name), strategy).map_err(|error| {
            Failure::error(format!(
                "cannot archive artifact {}: {error}",
                artifact.name
            ))
        })?;
        self.packed
            .insert((index, artifact.name.clone()), target.clone());
        Ok(target.to_string_lossy().into_owned())
    }

    pub fn take_deliveries(&mut self) -> Vec<Delivery> {
        let mut deliveries = std::mem::take(&mut self.deliveries);
        for child in &mut self.children {
            deliveries.extend(child.run.take_deliveries());
        }
        deliveries
    }

    pub fn artifact_delivery(&mut self, target: String, location: String) {
        self.deliveries.push(Delivery {
            location,
            target,
            mode: DeliveryMode::Copy,
        });
    }
}

/// A mode as Go reads an integer: decimal, or octal with a leading `0` or `0o`.
fn parse_mode(text: &str) -> Option<u32> {
    let text = text.trim();
    let octal = text.strip_prefix("0o").or_else(|| text.strip_prefix('0'));
    match octal {
        Some("") => Some(0),
        Some(digits) => u32::from_str_radix(digits, 8).ok(),
        None => text.parse().ok(),
    }
    .filter(|mode| *mode <= 0o7777)
}

/// Whether a file is a gzipped tar with at least one entry, as Argo's `isTarball` checks.
fn is_tarball(file: &Path) -> bool {
    let Ok(opened) = File::open(file) else {
        return false;
    };
    let mut archive = tar::Archive::new(GzDecoder::new(opened));
    let first = archive
        .entries()
        .ok()
        .and_then(|mut entries| entries.next());
    matches!(first, Some(Ok(_)))
}

/// Unpacks a gzipped tar or a zip file into a folder: its only entry is the artifact, or the
/// folder is when it holds several, as Argo's `unpack` decides.
fn unpacked(file: &Path, folder: &Path, zip: bool) -> io::Result<PathBuf> {
    let content = folder.join("content");
    std::fs::create_dir_all(&content)?;
    let result = if zip {
        unzip(file, &content)
    } else {
        untar(file, &content)
    };
    if let Err(error) = result.and_then(|()| check_links(&content, Path::new(""))) {
        let _ = std::fs::remove_dir_all(&content);
        return Err(error);
    }
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&content)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<io::Result<_>>()?;
    match entries.len() {
        1 => Ok(entries.remove(0)),
        _ => Ok(content),
    }
}

/// The most bytes and entries that one archive may unpack to, so that a small archive that
/// unpacks to a huge one cannot fill the disk.
const MOST_BYTES: u64 = 64 << 30;
const MOST_ENTRIES: u64 = 1_000_000;

fn oversized() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("the archive unpacks to more than {MOST_ENTRIES} entries or 64 GiB"),
    )
}

/// Unpacks a zip file within the limits; links are checked once all are written.
fn unzip(file: &Path, folder: &Path) -> io::Result<()> {
    let mut archive = zip::ZipArchive::new(File::open(file)?).map_err(io::Error::other)?;
    let mut size = 0u64;
    for index in 0..archive.len() {
        size += archive
            .by_index_raw(index)
            .map_err(io::Error::other)?
            .size();
    }
    if archive.len() as u64 > MOST_ENTRIES || size > MOST_BYTES {
        return Err(oversized());
    }
    archive.extract(folder).map_err(io::Error::other)
}

/// Unpacks a gzipped tar within the limits, refusing paths and links that lead out of the
/// folder, as Argo does; links count with the links unpacked before them.
fn untar(file: &Path, folder: &Path) -> io::Result<()> {
    let invalid = |message: String| io::Error::new(io::ErrorKind::InvalidData, message);
    let mut archive = tar::Archive::new(GzDecoder::new(File::open(file)?));
    let (mut size, mut count) = (0u64, 0u64);
    for entry in archive.entries()? {
        let mut entry = entry?;
        count += 1;
        size += entry.size();
        if count > MOST_ENTRIES || size > MOST_BYTES {
            return Err(oversized());
        }
        let path = entry.path()?.into_owned();
        if !entry.unpack_in(folder)? {
            return Err(invalid(format!("illegal file path: {}", path.display())));
        }
        if entry.header().entry_type().is_symlink() && resolved(folder, &path).is_none() {
            let link = entry.link_name()?.unwrap_or_default().into_owned();
            return Err(invalid(format!(
                "illegal symlink target: {} -> {}",
                path.display(),
                link.display()
            )));
        }
    }
    Ok(())
}

/// Fails when a link below `relative` leads out of `root`, which a later link can cause too.
fn check_links(root: &Path, relative: &Path) -> io::Result<()> {
    for entry in std::fs::read_dir(root.join(relative))? {
        let entry = entry?;
        let path = relative.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_symlink() && resolved(root, &path).is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("illegal symlink target: {}", path.display()),
            ));
        }
        if kind.is_dir() {
            check_links(root, &path)?;
        }
    }
    Ok(())
}

/// Where a path below `root` leads with its links followed, relative to `root`: `None` when
/// it leaves `root`, is absolute or loops. Missing parts count by name.
pub(crate) fn resolved(root: &Path, relative: &Path) -> Option<PathBuf> {
    let mut pending: Vec<PathBuf> = (relative.components().rev())
        .map(|part| PathBuf::from(part.as_os_str()))
        .collect();
    let (mut current, mut hops) = (PathBuf::new(), 0);
    while let Some(part) = pending.pop() {
        match part.components().next()? {
            std::path::Component::Normal(name) => {
                let next = current.join(name);
                let place = root.join(&next);
                if !std::fs::symlink_metadata(&place).is_ok_and(|meta| meta.is_symlink()) {
                    current = next;
                    continue;
                }
                hops += 1;
                if hops > 40 {
                    return None;
                }
                let target = std::fs::read_link(&place).ok()?;
                pending.extend(
                    (target.components().rev()).map(|part| PathBuf::from(part.as_os_str())),
                );
            }
            std::path::Component::ParentDir => {
                if !current.pop() {
                    return None;
                }
            }
            std::path::Component::CurDir => {}
            _ => return None,
        }
    }
    Some(current)
}

/// Copies a folder; links stay links, so that nothing is read through them.
fn copy_tree(source: &Path, target: &Path) -> io::Result<()> {
    std::fs::create_dir_all(target)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let to = target.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            std::os::unix::fs::symlink(std::fs::read_link(entry.path())?, &to)?;
        } else if kind.is_dir() {
            copy_tree(&entry.path(), &to)?;
        } else {
            std::fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}

/// Sets the mode of a folder and of everything below it, as `recurseMode` does.
fn set_modes(path: &Path, mode: u32) -> io::Result<()> {
    let kind = std::fs::symlink_metadata(path)?.file_type();
    if kind.is_symlink() {
        return Ok(());
    }
    if kind.is_dir() {
        for entry in std::fs::read_dir(path)? {
            set_modes(&entry?.path(), mode)?;
        }
        // Folders stay readable for the copy the task gets.
        let mode = mode | 0o700;
        return std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode | 0o400))
}
