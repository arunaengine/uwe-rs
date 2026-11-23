//! Runs the integration test modules of this crate in one test binary.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

#[path = "database.rs"]
mod database;
#[path = "insight.rs"]
mod insight;
#[path = "pinned.rs"]
mod pinned;
#[path = "steps.rs"]
mod steps;
#[path = "store.rs"]
mod store;
