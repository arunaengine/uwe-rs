//! Runs task attempts as Snowpark Container Services job services over the Snowflake SQL API,
//! with uwe's object-store staging and its helper.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub mod job;
pub mod jwt;
mod settings;

pub use jwt::KeyPair;
pub use settings::{Settings, connections_file, identifier, mappings};
