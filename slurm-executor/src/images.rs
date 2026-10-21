//! Coordinates atomic publication of shared Apptainer and Singularity SIF images.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use executor::registry::ImageConfig;
use executor::wrapper::quote;
use tokio::sync::{Mutex, OnceCell};

/// What the pull of an image gave: its SIF file, or why it could not be made.
pub type Made = Result<PathBuf, String>;

/// The SIF files of the images of a backend, each made once.
#[derive(Debug, Default)]
pub struct Images {
    made: Mutex<HashMap<String, Arc<OnceCell<Made>>>>,
}

impl Images {
    /// The cell of the SIF file of an image, which the first task of the image fills. The
    /// other tasks of the image wait for it and share what it holds, also a failed pull.
    pub async fn cell(&self, reference: &str) -> Arc<OnceCell<Made>> {
        let mut made = self.made.lock().await;
        made.entry(reference.to_owned()).or_default().clone()
    }
}

/// Names stable SIF files from pinned image digests or HTTP(S) address digests; otherwise
/// returns `None`.
pub fn file_name(reference: &str) -> Option<String> {
    if reference.starts_with("https://") || reference.starts_with("http://") {
        let digest = task_model::Digest::sha256(reference.as_bytes()).to_string();
        let hex = digest.rsplit(':').next().unwrap_or_default();
        return Some(format!("address-{}.sif", &hex[..hex.len().min(32)]));
    }
    if reference.contains("://") {
        return None;
    }
    let (_, digest) = reference.rsplit_once('@')?;
    let (algorithm, hex) = digest.split_once(':')?;
    let plain = |text: &str| !text.is_empty() && text.chars().all(|c| c.is_ascii_alphanumeric());
    (plain(algorithm) && plain(hex)).then(|| format!("{algorithm}-{hex}.sif"))
}

/// The name of the SIF file of an image of a registry named by a tag, like `debian:stable-slim`
/// as `tag-<digest of the name>.sif`.
pub fn tag_file_name(reference: &str) -> Option<String> {
    let local = reference.starts_with('/') || reference.ends_with(".sif");
    let other = reference.contains("://") || archive_path(reference).is_some();
    if reference.is_empty() || local || other || file_name(reference).is_some() {
        return None;
    }
    let digest = task_model::Digest::sha256(reference.as_bytes()).to_string();
    let hex = digest.rsplit(':').next().unwrap_or_default();
    Some(format!("tag-{}.sif", &hex[..hex.len().min(32)]))
}

/// The script of the job that pulls an image into `file`.
pub fn pull_script(program: &str, reference: &str, file: &Path, exit: &Path) -> String {
    let file = quote(&file.to_string_lossy());
    format!(
        "#!/bin/bash\n# The iunctus job that pulls an image into a SIF file once, for the jobs \
         of tasks.\npart={file}.part-$SLURM_JOB_ID\n{} pull --disable-cache \"$part\" {} && {{ sync \
         -- \"$part\" 2> /dev/null; mv -f \"$part\" {file}; }}\nstatus=$?\nrm -f \"$part\"\necho $status > {}\n",
        quote(program),
        quote(&source(reference)),
        quote(&exit.to_string_lossy())
    )
}

/// Where Apptainer pulls an image from: an address or an archive as it is, else the registry.
fn source(reference: &str) -> String {
    match reference.contains("://") || archive_path(reference).is_some() {
        true => reference.to_owned(),
        false => format!("docker://{reference}"),
    }
}

/// The hex digits of an image ID, like `sha256:<hex>`, by which tasks name a saved archive.
pub fn image_id(reference: &str) -> Option<&str> {
    let hex = reference.strip_prefix("sha256:")?;
    (hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit())).then_some(hex)
}

/// The local file of an image archive that Apptainer reads, like `docker-archive:/x/a.tar`.
pub fn archive_path(reference: &str) -> Option<&Path> {
    reference.strip_prefix("docker-archive:").map(Path::new)
}

/// The name of the SIF file of an image archive, and whether it lasts across runs: only for an
/// archive named by its image ID, like `sha256-<hex>.tar`, as iunctus saves them.
pub fn archive_file(reference: &str) -> Option<(String, bool)> {
    let path = archive_path(reference)?;
    let named = path.file_name().and_then(|name| name.to_str());
    let id = named.and_then(|name| name.strip_prefix("sha256-")?.strip_suffix(".tar"));
    let lasting =
        id.is_some_and(|hex| hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()));
    let digest = task_model::Digest::sha256(reference.as_bytes()).to_string();
    let hex = digest.rsplit(':').next().unwrap_or_default();
    Some((
        format!("archive-{}.sif", &hex[..hex.len().min(32)]),
        lasting,
    ))
}

/// The entrypoint, command and user of the image in a docker-archive tar, from its
/// `manifest.json` and the config that it names.
pub fn archive_config(path: &Path) -> Result<ImageConfig, String> {
    let config: serde_json::Value = serde_json::from_slice(&archive_parts(path)?)
        .map_err(|error| format!("the image config of {}: {error}", path.display()))?;
    let words = |key: &str| -> Vec<String> {
        let list = config["config"][key]
            .as_array()
            .cloned()
            .unwrap_or_default();
        list.iter()
            .filter_map(|word| word.as_str().map(str::to_owned))
            .collect()
    };
    Ok(ImageConfig {
        entrypoint: words("Entrypoint"),
        cmd: words("Cmd"),
        user: config["config"]["User"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
    })
}

/// The image ID of the image in a docker-archive tar, like `sha256:<hex>`: the digest of its
/// config.
pub fn archive_id(path: &Path) -> Result<String, String> {
    Ok(task_model::Digest::sha256(&archive_parts(path)?).to_string())
}

/// The config of the image in a docker-archive tar, as the bytes that its `manifest.json` names.
fn archive_parts(path: &Path) -> Result<Vec<u8>, String> {
    let failed = |error: std::io::Error| format!("cannot read {}: {error}", path.display());
    let mut archive = tar::Archive::new(std::fs::File::open(path).map_err(failed)?);
    // Configs and manifests are small; layers are skipped by seeking past them.
    let mut small = HashMap::new();
    for entry in archive.entries_with_seek().map_err(failed)? {
        let mut entry = entry.map_err(failed)?;
        if !entry.header().entry_type().is_file() || entry.size() > 1 << 20 {
            continue;
        }
        let name = entry.path().map_err(failed)?.to_string_lossy().into_owned();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).map_err(failed)?;
        small.insert(name.trim_start_matches("./").to_owned(), bytes);
    }
    let read = |name: &str| -> Result<serde_json::Value, String> {
        let bytes = small
            .get(name)
            .ok_or_else(|| format!("{} has no {name}", path.display()))?;
        serde_json::from_slice(bytes).map_err(|error| format!("{name}: {error}"))
    };
    let manifest = read("manifest.json")?;
    let config = (manifest[0]["Config"].as_str())
        .ok_or_else(|| format!("{} names no image config", path.display()))?;
    small
        .remove(config)
        .ok_or_else(|| format!("{} has no {config}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_shares_file() {
        assert_eq!(
            file_name("quay.io/biocontainers/fastqc@sha256:0ab9").as_deref(),
            Some("sha256-0ab9.sif")
        );
        assert_eq!(file_name("debian:stable-slim"), None);
        assert_eq!(file_name("/images/tool.sif"), None);
        assert_eq!(file_name("oras://host/tool@sha256:ab"), None);
        assert_eq!(file_name("debian@sha256:../x"), None);
        let address = "https://depot.galaxyproject.org/singularity/fastqc:0.12.1--hdfd78af_0";
        let name = file_name(address).expect("a file");
        assert!(
            name.starts_with("address-") && name.ends_with(".sif"),
            "{name}"
        );
        assert_eq!(name.len(), "address-.sif".len() + 32);
        assert_eq!(source(address), address);
    }

    #[test]
    fn tags_isolate_files() {
        let name = tag_file_name("debian:stable-slim").expect("a file");
        assert!(name.starts_with("tag-") && name.ends_with(".sif"), "{name}");
        assert_eq!(name.len(), "tag-.sif".len() + 32);
        assert_ne!(tag_file_name("debian:trixie-slim"), Some(name));
        assert!(tag_file_name("alpine").is_some());
        assert_eq!(tag_file_name("debian@sha256:ab"), None);
        assert_eq!(tag_file_name("/images/tool.sif"), None);
        assert_eq!(tag_file_name("tool.sif"), None);
        assert_eq!(tag_file_name("oras://host/tool:1"), None);
        assert_eq!(tag_file_name("https://host/tool.img"), None);
        assert_eq!(tag_file_name("docker-archive:/x/a.tar"), None);
    }

    #[test]
    fn publishes_whole_image() {
        let script = pull_script(
            "apptainer",
            "debian@sha256:ab",
            Path::new("/c/images/sha256-ab.sif"),
            Path::new("/c/images/sha256-ab/control/container-exit"),
        );
        let expected = "#!/bin/bash\n# The iunctus job that pulls an image into a SIF file \
            once, for the jobs of tasks.\npart='/c/images/sha256-ab.sif'.part-$SLURM_JOB_ID\n\
            'apptainer' pull --disable-cache \"$part\" 'docker://debian@sha256:ab' && { sync -- \
            \"$part\" 2> /dev/null; mv -f \"$part\" '/c/images/sha256-ab.sif'; }\nstatus=$?\nrm -f \"$part\"\necho $status > \
            '/c/images/sha256-ab/control/container-exit'\n";
        assert_eq!(script, expected);
    }
}
