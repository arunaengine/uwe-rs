//! Runs the integration test modules of this crate in one test binary.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

#![recursion_limit = "256"]

#[path = "cleanup_authority.rs"]
mod cleanup_authority;
#[path = "direct.rs"]
mod direct;
#[path = "directory_cache.rs"]
mod directory_cache;
#[path = "directory_retention.rs"]
mod directory_retention;
#[path = "hits.rs"]
mod hits;
#[path = "registry.rs"]
mod registry;
#[path = "remote_gc.rs"]
mod remote_gc;
#[path = "runner.rs"]
mod runner;
#[path = "runner_lease_loss.rs"]
mod runner_lease_loss;
#[path = "settlement.rs"]
mod settlement;
#[path = "sites.rs"]
mod sites;
#[path = "snapshot_permissions.rs"]
mod snapshot_permissions;
