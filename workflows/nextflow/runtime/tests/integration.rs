//! Runs the integration test modules of this crate in one test binary.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

#[path = "acceptance.rs"]
mod acceptance;
#[path = "adapter.rs"]
mod adapter;
#[path = "cleanup.rs"]
mod cleanup;
#[path = "driver.rs"]
mod driver;
#[path = "emits.rs"]
mod emits;
#[path = "errors.rs"]
mod errors;
#[path = "exec.rs"]
mod exec;
#[path = "export.rs"]
mod export;
#[path = "hooks.rs"]
mod hooks;
#[path = "legacy.rs"]
mod legacy;
#[path = "loader.rs"]
mod loader;
#[path = "observers.rs"]
mod observers;
#[path = "operators.rs"]
mod operators;
#[path = "plugins.rs"]
mod plugins;
#[path = "processes.rs"]
mod processes;
#[path = "publish.rs"]
mod publish;
#[path = "reports.rs"]
mod reports;
#[path = "schema.rs"]
mod schema;
#[path = "scratch.rs"]
mod scratch;
#[path = "store.rs"]
mod store;
#[path = "task.rs"]
mod task;
#[path = "typed.rs"]
mod typed;
#[path = "wave.rs"]
mod wave;
