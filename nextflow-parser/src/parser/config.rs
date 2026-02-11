//! Parses config files: assignments, blocks, selectors, plugin blocks and includes.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::Parser;
use crate::ast::Config;

impl Parser<'_> {
    /// The `compilationUnit` rule of config files.
    pub(super) fn config(&mut self) -> Config {
        Config {
            statements: Vec::new(),
        }
    }
}
