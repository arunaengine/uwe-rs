//! Runs wir documents through the execution graph of uwe: native blocks of other languages as
//! child drivers, and task ops as tasks.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod argo;
mod arrays;
mod callback;
mod child;
mod collecting;
pub mod cwl;
mod driver;
mod engine;
mod expr;
mod flows;
mod groovy;
mod grouping;
mod inputs;
mod javascript;
mod mixing;
pub mod nextflow;
#[allow(dead_code, reason = "the stream ops of the next commits use the rest")]
mod operator;
mod pairing;
mod plan;
mod python;
mod reducing;
mod routing;
pub mod snakemake;
#[allow(dead_code, reason = "the stream ops of the next commits use it")]
mod spill;
mod state;
mod streams;
mod task;
mod transforms;
mod values;
mod wdl;
mod wdl_closure;

pub use child::{Block, Child};
pub use driver::{Repositories, Settings, WirDriver, driver};
