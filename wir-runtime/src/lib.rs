//! Runs wir documents through the execution graph of uwe: native blocks of other languages as
//! child drivers, and task ops as tasks.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod child;
pub mod cwl;
mod driver;
mod inputs;
pub mod nextflow;
mod plan;
pub mod snakemake;
mod state;
mod task;

pub use child::{Block, Child};
pub use driver::{Settings, WirDriver, driver};
