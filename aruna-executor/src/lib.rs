//! Aruna native jobs and the host-supplied immutable object transfer boundary.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod backend;
pub mod bundle;
pub mod client;
pub mod objects;
pub mod states;

pub use backend::ArunaBackend;
