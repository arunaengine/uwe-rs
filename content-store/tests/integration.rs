//! Runs the integration test modules of this crate in one test binary.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

#[path = "directories.rs"]
mod directories;
#[path = "entry_repair.rs"]
mod entry_repair;
#[path = "entry_versions.rs"]
mod entry_versions;
#[path = "layers.rs"]
mod layers;
#[path = "lease_ownership.rs"]
mod lease_ownership;
#[path = "materialize.rs"]
mod materialize;
#[path = "store.rs"]
mod store;
