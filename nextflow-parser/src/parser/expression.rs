//! Parses expressions, call arguments and closure parameters.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::{Parse, Parser};
use crate::ast::{Argument, Expression, Parameter};

impl Parser<'_> {
    /// The `expression` rule.
    pub(super) fn expression(&mut self) -> Parse<Expression> {
        Err(self.error_here("Expressions are not supported yet"))
    }

    /// Turns a statement expression into a call without parentheses, like `cpus 4` or
    /// `tuple val(x), path(y)`, where the grammar allows it; otherwise returns it unchanged.
    pub(super) fn command_expression(&mut self, target: Expression) -> Parse<Expression> {
        Ok(target)
    }

    /// The `argumentList` rule: call arguments without parentheses.
    pub(super) fn argument_list(&mut self) -> Parse<Vec<Argument>> {
        Err(self.error_here("Arguments are not supported yet"))
    }

    /// The `formalParameterList` rule of closures and functions.
    pub(super) fn parameters(&mut self) -> Parse<Vec<Parameter>> {
        Err(self.error_here("Parameters are not supported yet"))
    }
}
