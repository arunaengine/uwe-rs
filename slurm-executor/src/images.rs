//! Coordinates atomic publication of shared Apptainer and Singularity SIF images.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
/// See `docs/design/engine.md` for the full contract.
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
/// See `docs/design/engine.md` for the full contract.
pub fn tag_file_name(reference: &str) -> Option<String> {
    let local = reference.starts_with('/') || reference.ends_with(".sif");
    if reference.is_empty() || local || reference.contains("://") || file_name(reference).is_some()
    {
        return None;
    }
    let digest = task_model::Digest::sha256(reference.as_bytes()).to_string();
    let hex = digest.rsplit(':').next().unwrap_or_default();
    Some(format!("tag-{}.sif", &hex[..hex.len().min(32)]))
}

/// The script of the job that pulls an image into `file`.
/// See `docs/design/engine.md` for the full contract.
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

/// Where Apptainer pulls an image from: an address as it is, else the registry.
fn source(reference: &str) -> String {
    match reference.contains("://") {
        true => reference.to_owned(),
        false => format!("docker://{reference}"),
    }
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
