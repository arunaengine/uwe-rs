//! The images of Apptainer and Singularity as SIF files, made once per image by a job of their
//! own. Jobs that each turn the same image of a registry into a SIF file at the same time spoil
//! the shared cache of Apptainer ("conveyor failed to get: unexpected end of JSON input"), and
//! each spends the time of its task on it. So the first task of an image waits for one job
//! that pulls it into the folder of images, and every job then runs that file.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use executor::wrapper::quote;
use tokio::sync::{Mutex, OnceCell};

/// The SIF files of the images of a backend, each made once.
#[derive(Debug, Default)]
pub struct Images {
    made: Mutex<HashMap<String, Arc<OnceCell<PathBuf>>>>,
}

impl Images {
    /// The cell of the SIF file of an image, which the first task of the image fills.
    pub async fn cell(&self, reference: &str) -> Arc<OnceCell<PathBuf>> {
        let mut made = self.made.lock().await;
        made.entry(reference.to_owned()).or_default().clone()
    }
}

/// The name of the SIF file of an image that is pinned to its digest, like
/// `debian@sha256:ab` as `sha256-ab.sif`, or of a SIF file at an `http://` or `https://`
/// address, like those that nf-core modules name for Apptainer, by the digest of its address;
/// `None` for any other image, which the jobs take as it is: a tag that may move, a path, or
/// an address with another scheme.
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

/// The name of the SIF file of an image of a registry named by a tag, like
/// `debian:stable-slim` as `tag-<digest of the name>.sif`. A tag may move, so a run never takes
/// such a file from an earlier run: it pulls the image again, once for all its jobs. `None` for
/// a path, a SIF file, an address with a scheme, and an image that [`file_name`] names.
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

/// The script of the job that pulls an image into `file`. It writes a file of its own first
/// and moves it into place at the end, so that no task reads half a file, and two runs that
/// pull the same image at once do not spoil each other's file. `exit` gets the exit code.
pub fn pull_script(program: &str, reference: &str, file: &Path, exit: &Path) -> String {
    let file = quote(&file.to_string_lossy());
    format!(
        "#!/bin/bash\n# The iunctus job that pulls an image into a SIF file once, for the jobs \
         of tasks.\npart={file}.part-$SLURM_JOB_ID\n{} pull --disable-cache \"$part\" {} && mv \
         -f \"$part\" {file}\nstatus=$?\nrm -f \"$part\"\necho $status > {}\n",
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
    fn only_images_pinned_to_their_digest_get_a_file() {
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
    fn images_named_by_a_tag_get_a_file_of_their_own() {
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
    fn the_pull_moves_a_whole_file_into_place() {
        let script = pull_script(
            "apptainer",
            "debian@sha256:ab",
            Path::new("/c/images/sha256-ab.sif"),
            Path::new("/c/images/sha256-ab/control/container-exit"),
        );
        let expected = "#!/bin/bash\n# The iunctus job that pulls an image into a SIF file \
            once, for the jobs of tasks.\npart='/c/images/sha256-ab.sif'.part-$SLURM_JOB_ID\n\
            'apptainer' pull --disable-cache \"$part\" 'docker://debian@sha256:ab' && mv -f \
            \"$part\" '/c/images/sha256-ab.sif'\nstatus=$?\nrm -f \"$part\"\necho $status > \
            '/c/images/sha256-ab/control/container-exit'\n";
        assert_eq!(script, expected);
    }
}
