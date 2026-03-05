//! Runs Nextflow workflows: the dataflow network of channels, operators and processes.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub mod channel;
mod network;

pub use network::Network;
