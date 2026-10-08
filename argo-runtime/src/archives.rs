//! Packs explicit output archive strategies while retaining native input files.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fs::File;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use argo_parser::model::Artifact;
use flate2::{Compression, write::GzEncoder};
use workflow_graph::{Delivery, DeliveryMode};
use zip::{ZipWriter, write::SimpleFileOptions};

use crate::run::{Failure, Result, Run, scalar_text};

enum Strategy {
    None,
    Tar(Compression),
    Zip,
}

#[cfg(test)]
#[path = "archives_tests.rs"]
mod tests;

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
                "archive compressionLevel -2 requires Huffman-only gzip, which iunctus does not support",
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
