//! Runs Galaxy tools and Galaxy workflows (`.ga`) through the execution graph: each tool run
//! is one task whose command the Cheetah template of the tool gives.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod driver;
mod job;
pub mod schema;
mod task;
pub mod workflow;

pub use driver::{GalaxyDriver, Options, driver, driver_with};
pub use job::Dataset;
pub use workflow::{is_tool, is_workflow};
