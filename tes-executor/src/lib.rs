//! The TES executor (decisions 8 and 22): each attempt of a task runs as one task on a GA4GH
//! TES v1.1 server, like Funnel or TESK, with the wrapper as its one executor.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub mod client;

pub use client::{Client, TesError};
