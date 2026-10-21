//! The TES executor (decisions 8 and 22): each attempt of a task runs as one task on a GA4GH
//! TES v1.1 server, like Funnel or TESK, with the wrapper as its one executor.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod backend;
mod capture;
pub mod client;
mod outputs;
mod settings;
mod staging;

pub use backend::TesBackend;
pub use client::{Client, TesError};
pub use settings::Settings;
pub use staging::ObjectStage;
