//! Runs task attempts through Kubernetes Jobs on nodes sharing task storage.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod backend;
mod cluster;
pub mod cws;
mod daemons;
mod manifest;
mod options;
mod orphans;
mod pods;
mod staging;

pub use backend::KubernetesBackend;
pub use cluster::Cluster;
pub use manifest::{
    DOWNLOAD_IMAGE, FOLDERS, Helper, LABEL, Limits, RUN, SharedVolume, Volumes, job, job_name,
};
pub use options::{PRIVILEGED, PodOptions};
pub use pods::{Look, Pods};
pub use staging::{Kubedal, PodStaging, Via};
