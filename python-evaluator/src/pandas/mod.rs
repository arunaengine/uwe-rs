//! A stand-in for the part of pandas that workflows use while they are planned: reading
//! sample sheets into tables, and looking values up in them.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::value::Value;

pub fn module() -> Vec<(&'static str, Value)> {
    Vec::new()
}
