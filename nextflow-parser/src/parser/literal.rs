//! Decodes string literals.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::{Parse, Parser};
use crate::ast::Name;
use crate::token::Token;

impl Parser<'_> {
    /// Decodes a `StringLiteral` token: quotes removed and escapes replaced.
    pub(super) fn string_value(&mut self, token: Token) -> String {
        self.text(token).to_owned()
    }

    /// A string literal used as a name, like an include source or a config selector target.
    pub(super) fn string_name(&mut self) -> Parse<Name> {
        Err(self.error_here("Strings are not supported yet"))
    }
}
