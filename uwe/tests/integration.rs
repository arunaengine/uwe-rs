//! Runs the integration test modules of this crate in one test binary.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;
mod targets;

#[path = "application.rs"]
mod application;
#[path = "argo_compiled.rs"]
mod argo_compiled;
#[path = "aws_batch.rs"]
mod aws_batch;
#[path = "azure_batch.rs"]
mod azure_batch;
#[path = "cache.rs"]
mod cache;
#[path = "catalogue.rs"]
mod catalogue;
#[path = "check.rs"]
mod check;
#[path = "cli_options.rs"]
mod cli_options;
#[path = "compiled.rs"]
mod compiled;
#[path = "conda.rs"]
mod conda;
#[path = "config_files.rs"]
mod config_files;
#[path = "config_test_workflows.rs"]
mod config_test_workflows;
#[path = "efficiency.rs"]
mod efficiency;
#[path = "embedding.rs"]
mod embedding;
#[path = "errors.rs"]
mod errors;
#[path = "flyte.rs"]
mod flyte;
#[path = "forms.rs"]
mod forms;
#[path = "google_batch.rs"]
mod google_batch;
#[path = "graph.rs"]
mod graph;
#[path = "grouped.rs"]
mod grouped;
#[path = "host_stores.rs"]
mod host_stores;
#[path = "hosted.rs"]
mod hosted;
#[path = "input_forms.rs"]
mod input_forms;
#[path = "inputs_test_workflows.rs"]
mod inputs_test_workflows;
#[path = "insight.rs"]
mod insight;
#[path = "isolation.rs"]
mod isolation;
#[path = "jarvice.rs"]
mod jarvice;
#[path = "lowering.rs"]
mod lowering;
#[path = "machine.rs"]
mod machine;
#[path = "native_acceptance.rs"]
mod native_acceptance;
#[path = "nextflow_compiler.rs"]
mod nextflow_compiler;
#[path = "nomad.rs"]
mod nomad;
#[path = "orphans.rs"]
mod orphans;
#[path = "publication.rs"]
mod publication;
#[path = "record.rs"]
mod record;
#[path = "repositories.rs"]
mod repositories;
#[path = "reproducibility.rs"]
mod reproducibility;
#[path = "run.rs"]
mod run;
#[path = "run_test_workflows.rs"]
mod run_test_workflows;
#[path = "runs.rs"]
mod runs;
#[path = "schedulers.rs"]
mod schedulers;
#[path = "secret_inputs.rs"]
mod secret_inputs;
#[path = "secrets.rs"]
mod secrets;
#[path = "serve.rs"]
mod serve;
#[path = "snowflake.rs"]
mod snowflake;
#[path = "storage.rs"]
mod storage;
#[path = "stub_test_workflows.rs"]
mod stub_test_workflows;
#[path = "support.rs"]
mod support;
#[path = "tools.rs"]
mod tools;
#[path = "wdl.rs"]
mod wdl;
#[path = "wdl_lowering.rs"]
mod wdl_lowering;
#[path = "wir.rs"]
mod wir;
#[path = "wr.rs"]
mod wr;
