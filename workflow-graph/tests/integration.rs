//! Runs the integration test modules of this crate in one test binary.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

#[path = "acceptance.rs"]
mod acceptance;
#[path = "attempts.rs"]
mod attempts;
#[path = "engine.rs"]
mod engine;
#[path = "events.rs"]
mod events;
#[path = "finalization.rs"]
mod finalization;
#[path = "order.rs"]
mod order;
#[path = "timers.rs"]
mod timers;
#[path = "validation.rs"]
mod validation;
