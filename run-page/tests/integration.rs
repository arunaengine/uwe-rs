//! Runs the integration test modules of this crate in one test binary.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

#[path = "authentication.rs"]
mod authentication;
#[path = "insight.rs"]
mod insight;
#[path = "live.rs"]
mod live;
#[path = "page.rs"]
mod page;
#[path = "report.rs"]
mod report;
#[path = "studio.rs"]
mod studio;
