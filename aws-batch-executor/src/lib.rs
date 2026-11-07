//! Runs task attempts as AWS Batch jobs, with uwe's object-store staging and its helper.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod backend;
mod client;
pub mod definition;
pub mod outcome;
mod settings;
pub mod submit;

pub use backend::BatchBackend;
pub use client::{BatchError, Client, LOG_GROUP};
pub use settings::{Settings, Volume};
