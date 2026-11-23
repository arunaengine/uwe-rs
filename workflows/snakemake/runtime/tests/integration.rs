//! Runs the integration test modules of this crate in one test binary.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

#[path = "adapter.rs"]
mod adapter;
#[path = "commands.rs"]
mod commands;
#[path = "conda.rs"]
mod conda;
#[path = "config.rs"]
mod config;
#[path = "excel.rs"]
mod excel;
#[path = "failures.rs"]
mod failures;
#[path = "gitfile.rs"]
mod gitfile;
#[path = "groups.rs"]
mod groups;
#[path = "hooks.rs"]
mod hooks;
#[path = "names.rs"]
mod names;
#[path = "pipes.rs"]
mod pipes;
#[path = "preambles.rs"]
mod preambles;
#[path = "profiles.rs"]
mod profiles;
#[path = "python_container.rs"]
mod python_container;
#[path = "rerun.rs"]
mod rerun;
#[path = "results.rs"]
mod results;
#[path = "run_blocks.rs"]
mod run_blocks;
#[path = "run_test_workflows.rs"]
mod run_test_workflows;
#[path = "schema.rs"]
mod schema;
#[path = "services.rs"]
mod services;
#[path = "shared.rs"]
mod shared;
#[path = "signature_params.rs"]
mod signature_params;
#[path = "signatures.rs"]
mod signatures;
#[path = "sources.rs"]
mod sources;
#[path = "storage.rs"]
mod storage;
#[path = "supported_options.rs"]
mod supported_options;
#[path = "target_controls.rs"]
mod target_controls;
#[path = "templates.rs"]
mod templates;
#[path = "triggers.rs"]
mod triggers;
#[path = "utilities.rs"]
mod utilities;
#[path = "workflows.rs"]
mod workflows;
