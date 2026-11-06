//! Runs task attempts as Flyte v2 runs of raw containers, with uwe's object-store staging and
//! its helper, over Flyte's Connect API with JSON messages.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod auth;
mod client;
mod settings;

pub use client::{Client, FlyteError};
pub use settings::{ApiKey, KEY_VARIABLES, Settings, address};
