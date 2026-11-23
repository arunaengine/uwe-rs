//! Runs the integration test modules of this crate in one test binary.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

#[path = "callbacks.rs"]
mod callbacks;
#[path = "compiler.rs"]
mod compiler;
#[path = "differential.rs"]
mod differential;
#[path = "outputs.rs"]
mod outputs;
#[path = "params.rs"]
mod params;
#[path = "scripts.rs"]
mod scripts;
