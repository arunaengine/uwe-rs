//! Runs the integration test modules of this crate in one test binary.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod records;

#[path = "load.rs"]
mod load;
#[path = "model.rs"]
mod model;
#[path = "test_workflows.rs"]
mod test_workflows;
#[path = "upgrade.rs"]
mod upgrade;
#[path = "web.rs"]
mod web;
