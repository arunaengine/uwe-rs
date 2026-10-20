//! Prepares CWL Docker recipes and archives for a selected local task engine.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::{Read, Write};
use std::path::Path;

use cwl_parser::iri;
use cwl_parser::model::Docker;
use executor::{ContainerEngine, ImageSource};
use sha2::{Digest, Sha256};

use crate::tool::Options;

pub(crate) fn select(docker: Option<(&Docker, bool)>, options: &Options) -> Result<String, String> {
    let Some((docker, required)) = docker else {
        return Ok(options.image.clone());
    };
    let source = if docker.docker_file.is_some() {
        ImageSource::Build
    } else if let Some(image) = &docker.docker_pull {
        return Ok(image.clone());
    } else if docker.docker_load.is_some() {
        ImageSource::Load
    } else if docker.docker_import.is_some() {
        ImageSource::Import
    } else {
        return Ok(docker
            .docker_image_id
            .clone()
            .unwrap_or_else(|| options.image.clone()));
    };
    if !options.simulated && options.local_engine.is_none() {
        return match required {
            true => Err(crate::tool::unsupported(
                "DockerRequirement build/load/import needs a local task executor; \
                remote executors need a configured registry push and a published manifest digest, \
                or a prepublished dockerPull image",
            )),
            false => Ok(options.image.clone()),
        };
    }
    let docker = docker.clone();
    let options = options.clone();
    std::thread::spawn(move || prepare(&docker, source, &options))
        .join()
        .map_err(|_| "image preparation worker failed".to_owned())?
}

fn prepare(docker: &Docker, source: ImageSource, options: &Options) -> Result<String, String> {
    let context;
    let path = match source {
        ImageSource::Build => {
            context = tempfile::NamedTempFile::new().map_err(|error| error.to_string())?;
            let mut builder =
                tar::Builder::new(context.reopen().map_err(|error| error.to_string())?);
            builder.mode(tar::HeaderMode::Deterministic);
            if let Some(url) = &docker.docker_context {
                let root = iri::file_path(url).ok_or("dockerContext must be a local directory")?;
                append(&mut builder, &root, &root).map_err(|error| error.to_string())?;
            }
            let bytes = docker.docker_file.as_deref().unwrap_or_default().as_bytes();
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, "Dockerfile", bytes)
                .map_err(|error| error.to_string())?;
            builder.finish().map_err(|error| error.to_string())?;
            context.path().to_owned()
        }
        ImageSource::Load | ImageSource::Import => {
            let url = match source {
                ImageSource::Load => docker.docker_load.as_ref(),
                _ => docker.docker_import.as_ref(),
            }
            .ok_or("image archive is missing")?;
            match iri::file_path(url) {
                Some(path) => path,
                None if options.simulated => {
                    return Ok(image_tag(source, &Sha256::digest(url.as_bytes())));
                }
                None => options
                    .remote
                    .storage
                    .blocking()
                    .fetch(url, &options.remote.downloads())
                    .map_err(|error| error.to_string())?,
            }
        }
    };
    let mut digest = Sha256::new();
    digest.update(format!("{source:?}\0"));
    if let Some(expected) = &docker.docker_image_id {
        digest.update(expected.as_bytes());
    }
    digest.update([0]);
    let mut file = std::fs::File::open(&path).map_err(|error| error.to_string())?;
    let mut bytes = [0; 65536];
    loop {
        let size = file.read(&mut bytes).map_err(|error| error.to_string())?;
        if size == 0 {
            break;
        }
        digest.update(&bytes[..size]);
    }
    let tag = image_tag(source, &digest.finalize());
    if options.simulated {
        return Ok(tag);
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(async {
        let engine = ContainerEngine::named(
            options
                .local_engine
                .as_deref()
                .filter(|name| !name.is_empty()),
        )
        .await
        .map_err(|error| error.to_string())?;
        engine
            .prepare_image(
                source,
                &path,
                &tag,
                docker.docker_image_id.as_deref(),
                &options.javascript.cancel,
            )
            .await
            .map_err(|error| error.to_string())
    })
}

fn append(builder: &mut tar::Builder<impl Write>, root: &Path, path: &Path) -> std::io::Result<()> {
    let mut entries = std::fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let name = path.strip_prefix(root).map_err(std::io::Error::other)?;
        if name == Path::new("Dockerfile") {
            continue;
        }
        builder.append_path_with_name(&path, name)?;
        if entry.file_type()?.is_dir() {
            append(builder, root, &path)?;
        }
    }
    Ok(())
}

fn image_tag(source: ImageSource, hash: &[u8]) -> String {
    let hex: String = hash.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("iunctus-cwl:{source:?}-{hex}").to_lowercase()
}
