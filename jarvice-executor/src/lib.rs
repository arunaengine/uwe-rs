//! Runs task attempts as JARVICE batch jobs, on the vault that uwe shares with the jobs or with
//! uwe's object-store staging and its helper.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod client;
mod settings;

pub use client::{Client, JarviceError, Submitted, output_lines};
pub use settings::{API_URL, Settings, VAULT_PATH};
