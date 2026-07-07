//! The Kubernetes executor: each attempt of a task runs as one Kubernetes Job with one pod,
//! which runs the wrapper in the task's image. The nodes see the store and the scratch
//! directories at the same paths, as `hostPath` volumes. Every call of `kubectl` names the
//! kubeconfig that the backend was given; there is no default.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod manifest;
mod pods;

pub use manifest::{LABEL, Limits, job, job_name};
pub use pods::{Kubectl, Look, Pods};
