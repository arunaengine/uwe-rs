//! Parses config files: assignments, blocks, selectors, plugin blocks and includes.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::{Parse, Parser, is_name};
use crate::Span;
use crate::ast::{Config, ConfigApply, ConfigStatement, Name, StatementKind};
use crate::token::TokenKind;

impl Parser<'_> {
    /// The `compilationUnit` rule of config files.
    pub(super) fn config(&mut self) -> Config {
        let (statements, _) =
            self.item_list(false, |_| false, |parser| parser.config_statement(false));
        Config { statements }
    }

    /// The `configStatement` rule, or `configBlockStatement` when `nested`, which adds selectors.
    fn config_statement(&mut self, nested: bool) -> Parse<ConfigStatement> {
        match self.peek() {
            TokenKind::IncludeConfig => {
                let start = self.bump().span;
                let path = self.expression()?;
                let span = self.span_from(start);
                Ok(ConfigStatement::Include { path, span })
            }
            TokenKind::If | TokenKind::Try | TokenKind::Def => self.invalid_statement(),
            TokenKind::CapitalizedIdentifier | TokenKind::PrimitiveType
                if self.at_declaration() =>
            {
                self.invalid_statement()
            }
            TokenKind::Identifier if nested && self.nth(1) == TokenKind::Colon => self.selector(),
            _ => self.config_path(),
        }
    }

    /// Whether a declaration with a type, like `String name = ''`, starts here.
    fn at_declaration(&self) -> bool {
        let array = self.nth(1) == TokenKind::LeftBracket && self.nth(2) == TokenKind::RightBracket;
        is_name(self.nth(1)) || self.nth(1) == TokenKind::Less || array
    }

    /// The `invalidStatement` rule: statements that scripts allow but config files do not.
    fn invalid_statement(&mut self) -> Parse<ConfigStatement> {
        let statement = self.statement()?;
        let message = match statement.kind {
            StatementKind::If { .. } => "If statements cannot be mixed with config statements",
            StatementKind::Try { .. } => "Try-catch blocks cannot be mixed with config statements",
            StatementKind::Declaration { .. } => {
                "Variable declarations cannot be mixed with config statements"
            }
            _ => "Invalid config statement",
        };
        Err(self.error_at(message, statement.span))
    }

    /// Assignments like `process.cpus = 2`, blocks like `env { ... }` and the incomplete
    /// statements of the `configIncomplete` rule, which all start with a path.
    fn config_path(&mut self) -> Parse<ConfigStatement> {
        let start = self.current().span;
        let mut path = vec![self.config_primary()?];
        let mut trailing = false;
        while self.eat(TokenKind::Dot) {
            if !self.at_primary() {
                trailing = true;
                break;
            }
            path.push(self.config_primary()?);
        }
        match self.peek_past_newlines() {
            TokenKind::Assign if !trailing => {
                self.skip_newlines();
                self.bump();
                self.skip_newlines();
                let value = self.expression()?;
                let span = self.span_from(start);
                Ok(ConfigStatement::Assign { path, value, span })
            }
            TokenKind::LeftBrace if !trailing && path.len() == 1 => {
                let name = path.remove(0);
                self.config_block(name, start)
            }
            _ if self.at_separator()
                || matches!(self.peek(), TokenKind::RightBrace | TokenKind::EndOfFile) =>
            {
                Err(self.error_at("Incomplete statement", self.span_from(start)))
            }
            _ => Err(self.unexpected()),
        }
    }

    /// The `configPrimary` rule: a name, a string or a primitive type name.
    fn config_primary(&mut self) -> Parse<Name> {
        match self.peek() {
            TokenKind::StringLiteral => self.string_name(),
            TokenKind::PrimitiveType => {
                let token = self.bump();
                let text = self.text(token).to_owned();
                Ok(Name {
                    text,
                    span: token.span,
                })
            }
            _ => self.name(),
        }
    }

    fn at_primary(&self) -> bool {
        let literal = matches!(
            self.peek(),
            TokenKind::StringLiteral | TokenKind::PrimitiveType
        );
        literal || self.at_name()
    }

    /// The `configBlock` and `configApplyBlock` rules, told apart by the first statement.
    fn config_block(&mut self, name: Name, start: Span) -> Parse<ConfigStatement> {
        self.skip_newlines();
        // The opening brace.
        self.bump();
        self.skip_newlines();
        if !self.at_apply() {
            let body = self.config_body()?;
            let span = self.span_from(start);
            return Ok(ConfigStatement::Block { name, body, span });
        }
        let (calls, _) = self.item_list(true, |_| false, Self::config_apply);
        self.expect(TokenKind::RightBrace, "`}`")?;
        let span = self.span_from(start);
        if name.text != "plugins" {
            let message = "Config settings must be assigned with an equals sign (`=`)";
            self.error_at(message, span);
        }
        Ok(ConfigStatement::Apply { name, calls, span })
    }

    /// Whether a block holds calls like `id 'nf-schema@2.1.0'`: a name followed by an argument
    /// rather than by what an assignment, block, selector or include would need.
    fn at_apply(&self) -> bool {
        let start = match self.peek() {
            TokenKind::Identifier | TokenKind::In => true,
            TokenKind::CapitalizedIdentifier => !self.at_declaration(),
            _ => false,
        };
        let follows = matches!(
            self.nth(1),
            TokenKind::Assign
                | TokenKind::Dot
                | TokenKind::LeftBrace
                | TokenKind::Colon
                | TokenKind::Newline
                | TokenKind::Semicolon
                | TokenKind::RightBrace
                | TokenKind::EndOfFile
        );
        start && !follows
    }

    /// The statements of a block after its opening brace, and the closing brace.
    fn config_body(&mut self) -> Parse<Vec<ConfigStatement>> {
        let (body, _) = self.item_list(true, |_| false, |parser| parser.config_statement(true));
        self.expect(TokenKind::RightBrace, "`}`")?;
        Ok(body)
    }

    /// The `configApply` rule: a name and call arguments without parentheses.
    fn config_apply(&mut self) -> Parse<ConfigApply> {
        let start = self.current().span;
        let name = self.name()?;
        let arguments = self.argument_list()?;
        let span = self.span_from(start);
        Ok(ConfigApply {
            name,
            arguments,
            span,
        })
    }

    /// The `configSelector` rule, like `withLabel: 'process_high' { ... }`.
    fn selector(&mut self) -> Parse<ConfigStatement> {
        let start = self.current().span;
        let kind = self.name()?;
        // The colon.
        self.bump();
        let target = self.config_primary()?;
        self.skip_newlines();
        self.expect(TokenKind::LeftBrace, "`{`")?;
        self.skip_newlines();
        let body = self.config_body()?;
        let span = self.span_from(start);
        Ok(ConfigStatement::Selector {
            kind,
            target,
            body,
            span,
        })
    }
}
