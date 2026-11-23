//! Runs the integration test modules of this crate in one test binary.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

#[path = "global.rs"]
mod global;
#[path = "module.rs"]
mod module;
#[path = "python.rs"]
mod python;
#[path = "rule.rs"]
mod rule;
#[path = "test_workflows.rs"]
mod test_workflows;
