//! Runs wir documents through the execution graph of uwe: native blocks of other languages as
//! child drivers, and task ops as tasks.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub mod adapter;
pub mod argo;
mod arrays;
mod buffering;
mod callback;
mod child;
mod collecting;
mod control;
pub mod cwl;
mod driver;
mod effects;
mod engine;
mod expr;
mod flows;
mod groovy;
mod grouping;
mod inputs;
mod javascript;
mod looping;
mod mixing;
pub mod nextflow;
mod nextflow_scope;
#[allow(dead_code, reason = "the stream ops of the next commits use the rest")]
mod operator;
mod pairing;
mod plan;
mod processes;
mod publishing;
mod python;
mod reducing;
mod regions;
mod routing;
mod selecting;
pub mod snakemake;
#[allow(dead_code, reason = "the stream ops of the next commits use it")]
mod spill;
mod splitting;
mod state;
mod streams;
mod task;
mod transforms;
mod values;
mod wdl;
mod writing;

pub use child::{Block, Child};
pub use driver::{Printer, Repositories, Settings, WirDriver, driver};
