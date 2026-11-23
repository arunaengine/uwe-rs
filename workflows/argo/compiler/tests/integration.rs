//! Runs the integration test modules of this crate in one test binary.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

#[path = "compile.rs"]
mod compile;
#[path = "conformance.rs"]
mod conformance;
#[path = "runs.rs"]
mod runs;
