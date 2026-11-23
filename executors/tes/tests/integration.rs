//! Runs the integration test modules of this crate in one test binary.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

#[path = "fake_server.rs"]
mod fake_server;
#[path = "funnel.rs"]
mod funnel;
#[path = "nf_ga4gh.rs"]
mod nf_ga4gh;
