//! Reads the labeled sections shared by processes, agents and workflows.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::Parser;
use crate::Span;
use crate::ast::{Block, Statement};

impl Parser<'_> {
    /// The statements of a section as a block, like the body after `script:`.
    pub(super) fn labeled_block(&mut self, is_label: fn(&Self) -> bool) -> Block {
        let start = self.current().span;
        section_block(self.section_statements(is_label), start)
    }

    /// The statements of one section, up to the next label or the closing brace.
    pub(super) fn section_statements(&mut self, is_label: fn(&Self) -> bool) -> Vec<Statement> {
        let statements = self.statement_sequence(is_label).unwrap_or_default();
        if !self.at_list_end(true) && !is_label(self) {
            // The sequence stopped early after an error; go on at the next label.
            self.skip_section(is_label);
        }
        statements
    }

    /// Skips to the next section label or the closing brace, after an error in a section.
    pub(super) fn skip_section(&mut self, is_label: fn(&Self) -> bool) {
        loop {
            self.recover();
            if self.skip_separators() {
                if is_label(self) {
                    return;
                }
            } else if self.at_list_end(true) {
                return;
            } else {
                // A closing bracket that matches nothing in this section.
                self.bump();
            }
        }
    }
}

/// Checks section labels against one or more allowed orders, given as a rank per label.
pub(super) struct LabelOrder<const N: usize> {
    last: [Option<u8>; N],
    pub(super) valid: [bool; N],
}

impl<const N: usize> LabelOrder<N> {
    pub(super) fn new() -> Self {
        Self {
            last: [None; N],
            valid: [true; N],
        }
    }

    /// Records the next label and tells whether some order still allows all labels so far.
    pub(super) fn accept(&mut self, ranks: [u8; N]) -> bool {
        for (index, rank) in ranks.into_iter().enumerate() {
            if self.last[index].is_some_and(|last| last >= rank) {
                self.valid[index] = false;
            }
            self.last[index] = Some(rank);
        }
        self.valid.contains(&true)
    }
}

/// A block made of the statements of a section, spanning from the first to the last statement,
/// or empty at `start` when there are none.
pub(super) fn section_block(statements: Vec<Statement>, start: Span) -> Block {
    let span = match (statements.first(), statements.last()) {
        (Some(first), Some(last)) => first.span.to(last.span),
        _ => Span {
            start: start.start,
            end: start.start,
        },
    };
    Block { statements, span }
}

/// The number of statements in a section and the span of the last one.
pub(super) fn section_end(statements: &[Statement], start: Span) -> (usize, Span) {
    let last = statements.last().map_or(start, |statement| statement.span);
    (statements.len(), last)
}
