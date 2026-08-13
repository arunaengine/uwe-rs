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
/// `debian@sha256:ab` as `sha256-ab.sif`; `None` for any other image, which the jobs take as
/// it is: a tag that may move, a path, or an address with its own scheme.
pub fn file_name(reference: &str) -> Option<String> {
    if reference.contains("://") {
        return None;
    }
    let (_, digest) = reference.rsplit_once('@')?;
    let (algorithm, hex) = digest.split_once(':')?;
    let plain = |text: &str| !text.is_empty() && text.chars().all(|c| c.is_ascii_alphanumeric());
    (plain(algorithm) && plain(hex)).then(|| format!("{algorithm}-{hex}.sif"))
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
        quote(&format!("docker://{reference}")),
        quote(&exit.to_string_lossy())
    )
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
