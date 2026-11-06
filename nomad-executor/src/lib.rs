//! Runs task attempts as HashiCorp Nomad batch jobs, on shared folders or with uwe's
//! object-store staging and its helper.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod backend;
mod client;
pub mod job;
pub mod options;
pub mod outcome;
mod settings;

pub use backend::NomadBackend;
pub use client::{Client, NomadError};
pub use settings::{
    ADDRESS, Cleanup, CpuMode, Hidden, Settings, Volume, affinity, cleanup_mode, constraint,
    cpu_mode, driver_name, rules, spread, volume_list,
};

/// Why uwe refuses privileged containers, which nf-nomad starts by default.
pub const PRIVILEGED: &str = "uwe runs containers with the least privileges and never starts \
                              privileged ones";
