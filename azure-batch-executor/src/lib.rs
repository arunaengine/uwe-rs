//! Runs task attempts as Azure Batch tasks, with uwe's object-store staging and its helper.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub mod auth;
mod client;
pub mod jobs;
pub mod outcome;
pub mod pool;
mod settings;
pub mod task;

pub use auth::{Credentials, Source};
pub use client::{BatchError, Client};
pub use settings::{
    API_VERSION, AUTO, Hidden, Identity, PRIVILEGED, PoolSettings, Registry, Settings,
};
