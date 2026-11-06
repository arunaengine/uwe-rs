//! Runs task attempts as Google Batch jobs, with uwe's object-store staging and its helper.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod client;
mod settings;
pub mod token;

pub use client::{Client, GoogleError};
pub use settings::{LOCATION, PREEMPTED, Settings};
pub use token::{Credentials, Source, TokenError};
