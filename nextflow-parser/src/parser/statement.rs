//! Parses statements and blocks.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::{Parse, Parser};
use crate::ast::{Block, Statement};

impl Parser<'_> {
    /// The `statement` rule.
    pub(super) fn statement(&mut self) -> Parse<Statement> {
        Err(self.error_here("Statements are not supported yet"))
    }

    /// A braced block: `{` then statements then `}`.
    pub(super) fn block(&mut self) -> Parse<Block> {
        Err(self.error_here("Blocks are not supported yet"))
    }

    /// Statements separated by newlines or semicolons, up to a closing brace, the end of
    /// input, or a statement start where `is_end` holds, such as a section label.
    pub(super) fn statement_sequence(
        &mut self,
        is_end: impl Fn(&Self) -> bool,
    ) -> Parse<Vec<Statement>> {
        let _ = is_end;
        Err(self.error_here("Statements are not supported yet"))
    }
}
