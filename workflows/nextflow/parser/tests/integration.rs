//! Runs the integration test modules of this crate in one test binary.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

#[path = "config.rs"]
mod config;
#[path = "legacy.rs"]
mod legacy;
#[path = "lexer.rs"]
mod lexer;
#[path = "script.rs"]
mod script;
#[path = "test_workflows.rs"]
mod test_workflows;
