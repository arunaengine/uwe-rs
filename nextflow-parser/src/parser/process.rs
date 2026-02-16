//! Parses process and agent definitions: sections, inputs, outputs and their checks.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::{Parse, Parser};
use crate::Span;
use crate::ast::{
    BinaryOperator, ExpressionKind, NamedOutput, ProcessInput, Statement, StatementKind, TypedName,
};
use crate::token::TokenKind;

impl Parser<'_> {
    /// The `processInput` rule: a typed name, `record(...)` or `tuple(...)` of typed names, or a
    /// legacy input statement like `tuple val(meta), path(reads)`.
    pub(super) fn process_input(&mut self) -> Parse<ProcessInput> {
        let start = self.current().span;
        let grouped = matches!(self.peek(), TokenKind::Record | TokenKind::Tuple);
        if grouped && self.nth(1) == TokenKind::LeftParen {
            let tuple = self.at(TokenKind::Tuple);
            if let Some(fields) = self.speculate(Self::field_list) {
                if !tuple {
                    return Ok(ProcessInput::Record(fields));
                }
                if fields.len() < 2 {
                    let message = "Process tuple input must have more than one component";
                    self.error_at(message, self.span_from(start));
                }
                return Ok(ProcessInput::Tuple(fields));
            }
        }
        if let Some(input) = self.speculate(Self::line_name) {
            return Ok(ProcessInput::Named(input));
        }
        Ok(ProcessInput::Statement(self.statement()?))
    }

    /// The typed names of `record(...)` or `tuple(...)`, which must fill the whole line.
    fn field_list(&mut self) -> Parse<Vec<TypedName>> {
        // The keyword and the opening parenthesis.
        self.bump();
        self.bump();
        self.skip_newlines();
        let mut fields = vec![self.typed_name()?];
        while self.eat(TokenKind::Comma) {
            self.skip_newlines();
            if self.at(TokenKind::RightParen) {
                break;
            }
            fields.push(self.typed_name()?);
        }
        self.skip_newlines();
        self.expect(TokenKind::RightParen, "`)`")?;
        self.line_end()?;
        Ok(fields)
    }

    /// A typed name that fills the whole line, like `reads: Path`.
    pub(super) fn line_name(&mut self) -> Parse<TypedName> {
        let name = self.typed_name()?;
        self.line_end()?;
        Ok(name)
    }

    /// The `processOutput` and `workflowEmit` rules: `name`, `name: Type` or `name = value` when
    /// that fills the whole line, else a statement.
    pub(super) fn named_output(&mut self) -> Parse<NamedOutput> {
        let named = self.speculate(|parser| {
            let target = parser.typed_name()?;
            let mut value = None;
            if parser.eat(TokenKind::Assign) {
                value = Some(parser.expression()?);
            }
            parser.line_end()?;
            Ok(NamedOutput::Named { target, value })
        });
        match named {
            Some(output) => Ok(output),
            None => Ok(NamedOutput::Statement(self.statement()?)),
        }
    }

    /// Reports statements that cannot be directives. Like the official `checkDirective`, it
    /// accepts a name, a call without a target like `cpus 4`, and a name minus a value.
    pub(super) fn check_directives(&mut self, statements: &[Statement], message: &str) {
        for statement in statements {
            let valid = match &statement.kind {
                StatementKind::Expression(expression) => match &expression.kind {
                    ExpressionKind::Variable(_) => true,
                    ExpressionKind::MethodCall { target, .. } => target.is_none(),
                    ExpressionKind::Binary {
                        operator: BinaryOperator::Subtract,
                        left,
                        ..
                    } => matches!(left.kind, ExpressionKind::Variable(_)),
                    _ => false,
                },
                StatementKind::Empty => true,
                _ => false,
            };
            if !valid {
                self.error_at(message, statement.span);
            }
        }
    }

    /// Declarations and control flow cannot declare inputs or outputs, in legacy or typed
    /// processes alike.
    pub(super) fn check_declarations<'s>(
        &mut self,
        statements: impl Iterator<Item = &'s Statement>,
        message: &str,
    ) {
        let spans: Vec<Span> = statements
            .filter(|statement| {
                !matches!(
                    statement.kind,
                    StatementKind::Expression(_) | StatementKind::Empty
                )
            })
            .map(|statement| statement.span)
            .collect();
        for span in spans {
            self.error_at(message, span);
        }
    }

    /// Reports topic statements that are not of the form `value >> 'topic'`.
    pub(super) fn check_topics(&mut self, statements: &[Statement]) {
        for statement in statements {
            let valid = match &statement.kind {
                StatementKind::Expression(expression) => matches!(
                    expression.kind,
                    ExpressionKind::Binary {
                        operator: BinaryOperator::RightShift,
                        ..
                    }
                ),
                _ => false,
            };
            if !valid {
                self.error_at("Invalid process topic statement", statement.span);
            }
        }
    }
}
