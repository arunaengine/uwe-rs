//! Runs task attempts as HashiCorp Nomad batch jobs, on shared folders or with uwe's
//! object-store staging and its helper.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod client;
mod settings;

pub use client::{Client, NomadError};
pub use settings::{
    ADDRESS, Cleanup, CpuMode, Hidden, Settings, Volume, affinity, cleanup_mode, constraint,
    cpu_mode, driver_name, rules, spread, volume_list,
};
