//! Runs the integration test modules of this crate in one test binary.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod order;
#[path = "../../parser/tests/records/mod.rs"]
mod records;

#[path = "conformance.rs"]
mod conformance;
#[path = "contracts.rs"]
mod contracts;
#[path = "directory_updates.rs"]
mod directory_updates;
#[path = "driver.rs"]
mod driver;
#[path = "formats.rs"]
mod formats;
#[path = "graph_test_workflows.rs"]
mod graph_test_workflows;
#[path = "images.rs"]
mod images;
#[path = "in_place_admission.rs"]
mod in_place_admission;
#[path = "inputs_test_workflows.rs"]
mod inputs_test_workflows;
#[path = "javascript_helper.rs"]
mod javascript_helper;
#[path = "job_test_workflows.rs"]
mod job_test_workflows;
#[path = "output_globs.rs"]
mod output_globs;
#[path = "output_paths.rs"]
mod output_paths;
#[path = "schema.rs"]
mod schema;
#[path = "shm.rs"]
mod shm;
#[path = "text_inputs.rs"]
mod text_inputs;
