//! How values move through a port: one bound value or a stream of items.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde::{Deserialize, Serialize};

/// The flow kind of a port. Both ends of an edge have the same kind; the full channel
/// contract is in the "Channel contract" section of the crate README.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Flow {
    /// One value, bound once and never changed. Every reader gets the same value, as often as
    /// it reads it. A bound null is a value; a producer that fails binds nothing.
    #[default]
    Value,
    /// Items in the producer's order, then a close. Every reader sees every item; a stream that
    /// closes with no items is empty, not null. Reserved: no op of this version reads streams.
    Stream,
}

impl Flow {
    pub fn is_value(&self) -> bool {
        *self == Flow::Value
    }
}
