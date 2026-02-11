//! Parses script declarations: includes, parameters, processes, workflows and functions.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::Parser;
use crate::ast::Script;

impl Parser<'_> {
    /// The `compilationUnit` rule of scripts.
    pub(super) fn script(&mut self) -> Script {
        Script { items: Vec::new() }
    }
}
