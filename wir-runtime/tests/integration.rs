//! Runs the integration test modules of this crate in one test binary.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

#[path = "acceptance.rs"]
mod acceptance;
#[path = "adapters.rs"]
mod adapters;
#[path = "argo.rs"]
mod argo;
#[path = "branch.rs"]
mod branch;
#[path = "checkpoints.rs"]
mod checkpoints;
#[path = "child.rs"]
mod child;
#[path = "defaults.rs"]
mod defaults;
#[path = "emission.rs"]
mod emission;
#[path = "expr.rs"]
mod expr;
#[path = "formats.rs"]
mod formats;
#[path = "groovy.rs"]
mod groovy;
#[path = "hooks.rs"]
mod hooks;
#[path = "load.rs"]
mod load;
#[path = "loops.rs"]
mod loops;
#[path = "map.rs"]
mod map;
#[path = "native.rs"]
mod native;
#[path = "nextflow.rs"]
mod nextflow;
#[path = "processes.rs"]
mod processes;
#[path = "regions.rs"]
mod regions;
#[path = "rules.rs"]
mod rules;
#[path = "secondary.rs"]
mod secondary;
#[path = "select.rs"]
mod select;
#[path = "snakemake.rs"]
mod snakemake;
#[path = "streams.rs"]
mod streams;
#[path = "task.rs"]
mod task;
#[path = "transforms.rs"]
mod transforms;
#[path = "wdl.rs"]
mod wdl;
