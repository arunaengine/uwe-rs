//! Runs task attempts through Kubernetes Jobs on nodes sharing task storage.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod backend;
mod cluster;
mod manifest;
mod pods;
mod staging;

pub use backend::KubernetesBackend;
pub use cluster::Cluster;
pub use manifest::{FOLDERS, Helper, LABEL, Limits, Volumes, job, job_name};
pub use pods::{Look, Pods};
pub use staging::{Kubedal, PodStaging, Via};
