//! Runs wir documents through the execution graph of uwe: native blocks of other languages as
//! child drivers, and task ops as tasks.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod argo;
mod arrays;
mod child;
pub mod cwl;
mod driver;
mod expr;
mod flows;
mod groovy;
mod inputs;
pub mod nextflow;
#[allow(dead_code, reason = "the stream ops of the next commits use the rest")]
mod operator;
mod plan;
pub mod snakemake;
#[allow(dead_code, reason = "the stream ops of the next commits use it")]
mod spill;
mod state;
mod streams;
mod task;
mod wdl;

pub use child::{Block, Child};
pub use driver::{Repositories, Settings, WirDriver, driver};
