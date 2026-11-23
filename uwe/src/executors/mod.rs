//! The executors of uwe: one module per backend that builds its runner from the setup, and the
//! Conda images and file staging that several backends share.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub(crate) mod aws_batch;
pub(crate) mod azure_batch;
pub(crate) mod conda;
pub(crate) mod flyte;
pub(crate) mod google_batch;
pub(crate) mod grid;
pub(crate) mod jarvice;
pub(crate) mod kubernetes;
pub(crate) mod local;
pub(crate) mod nomad;
pub(crate) mod slurm;
pub(crate) mod snowflake;
pub(crate) mod staging;
pub(crate) mod tes;
pub(crate) mod wr;
