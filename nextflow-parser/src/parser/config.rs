//! Parses config files: assignments, blocks, selectors, plugin blocks and includes.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::{Parse, Parser, is_name};
use crate::ast::{Config, ConfigApply, ConfigCatch, ConfigStatement, Item, Name, StatementKind};
use crate::token::TokenKind;
use crate::{Diagnostic, Span};

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
            TokenKind::If => self.legacy_if(nested),
            TokenKind::Try => self.legacy_try(nested),
            TokenKind::Def => self.legacy_definition(),
            TokenKind::CapitalizedIdentifier | TokenKind::PrimitiveType
                if self.at_declaration() =>
            {
                self.legacy_definition()
            }
            TokenKind::Identifier if self.nth(1) == TokenKind::Colon => self.selector(nested),
            _ => self.config_path(),
        }
    }

    /// Whether a declaration with a type, like `String name = ''`, starts here.
    fn at_declaration(&self) -> bool {
        let array = self.nth(1) == TokenKind::LeftBracket && self.nth(2) == TokenKind::RightBracket;
        is_name(self.nth(1)) || self.nth(1) == TokenKind::Less || array
    }

    /// Assignments like `process.cpus = 2`, blocks like `env { ... }` and the incomplete
    /// statements of the `configIncomplete` rule, which all start with a path.
    fn config_path(&mut self) -> Parse<ConfigStatement> {
        let position = self.position;
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
                // Legacy syntax: a chain like `a = b = 5`.
                let value = self.assignment_chain(value)?;
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
            _ => self.legacy_statement(position),
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
    /// Legacy syntax also has selectors outside blocks, and labeled statements like
    /// `saveAs: { ... }`.
    fn selector(&mut self, nested: bool) -> Parse<ConfigStatement> {
        let (start, position) = (self.current().span, self.position);
        let kind = self.name()?;
        let colon = self.bump().span;
        if !self.at_primary() {
            return self.legacy_labeled(position);
        }
        if !nested {
            self.legacy
                .push(Diagnostic::new("Unexpected input: ':'", colon));
        }
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

    /// A labeled statement like `saveAs: { ... }` that starts at token `position`, which
    /// Groovy runs as the statement after the label.
    fn legacy_labeled(&mut self, position: usize) -> Parse<ConfigStatement> {
        let failure = self.position;
        self.position = position;
        let statement = self.speculate(Self::labeled_statement);
        let ends = self.at_separator() || self.at_list_end(true);
        let end = self.position;
        self.position = failure;
        let Some(statement) = statement.filter(|_| ends) else {
            return Err(self.error_here("Expected a name"));
        };
        let errors = self.errors.len();
        self.error_here("Expected a name");
        self.legacy.extend(self.errors.split_off(errors));
        self.position = end;
        Ok(ConfigStatement::Statement(statement))
    }

    /// A legacy `if` statement around config statements, with `else if` and `else`.
    fn legacy_if(&mut self, nested: bool) -> Parse<ConfigStatement> {
        let start = self.bump().span;
        self.expect(TokenKind::LeftParen, "'(' after 'if'")?;
        self.skip_newlines();
        let condition = self.expression()?;
        self.skip_newlines();
        self.expect(TokenKind::RightParen, "')'")?;
        self.skip_newlines();
        let then = self.legacy_body(nested)?;
        let mut otherwise = Vec::new();
        if self.peek_past_newlines() == TokenKind::Else {
            self.skip_newlines();
            self.bump();
            self.skip_newlines();
            otherwise = match self.peek() {
                TokenKind::If => vec![self.legacy_if(nested)?],
                _ => self.legacy_body(nested)?,
            };
        }
        let span = self.span_from(start);
        let message = "If statements cannot be mixed with config statements";
        self.legacy.push(Diagnostic::new(message, span));
        Ok(ConfigStatement::If {
            condition,
            then,
            otherwise,
            span,
        })
    }

    /// A legacy `try` statement around config statements, with its `catch` clauses.
    fn legacy_try(&mut self, nested: bool) -> Parse<ConfigStatement> {
        let start = self.bump().span;
        self.skip_newlines();
        let body = self.legacy_body(nested)?;
        let mut catches = Vec::new();
        while self.peek_past_newlines() == TokenKind::Catch {
            self.skip_newlines();
            self.bump();
            self.expect(TokenKind::LeftParen, "'(' after 'catch'")?;
            let (name, types) = if self.at_name() && self.nth(1) == TokenKind::RightParen {
                (self.variable_name()?, Vec::new())
            } else {
                let types = self.catch_types()?;
                (self.variable_name()?, types)
            };
            self.expect(TokenKind::RightParen, "')'")?;
            self.skip_newlines();
            let body = self.legacy_body(nested)?;
            catches.push(ConfigCatch { name, types, body });
        }
        let span = self.span_from(start);
        let message = "Try-catch blocks cannot be mixed with config statements";
        self.legacy.push(Diagnostic::new(message, span));
        Ok(ConfigStatement::Try {
            body,
            catches,
            span,
        })
    }

    /// The body of a legacy `if`, `else`, `try` or `catch`: config statements in braces, or
    /// a single one.
    fn legacy_body(&mut self, nested: bool) -> Parse<Vec<ConfigStatement>> {
        if !self.eat(TokenKind::LeftBrace) {
            return Ok(vec![self.config_statement(nested)?]);
        }
        let (body, _) = self.item_list(true, |_| false, |parser| parser.config_statement(nested));
        self.expect(TokenKind::RightBrace, "`}`")?;
        Ok(body)
    }

    /// A legacy function like `def check_max(obj, type) { ... }`, or a local variable like
    /// `def timestamp = new Date().format('yyyy')`.
    fn legacy_definition(&mut self) -> Parse<ConfigStatement> {
        let start = self.current().span;
        if let Some(head) = self.speculate(Self::function_head) {
            // Strict syntax stops at the parenthesis after the name.
            let parenthesis = self.tokens[self.position - 1].span;
            let Item::Function(function) = self.function(start, head)? else {
                unreachable!("the function rule gives a function")
            };
            self.legacy
                .push(Diagnostic::new("Unexpected input: '('", parenthesis));
            return Ok(ConfigStatement::Function(function));
        }
        let statement = self.statement()?;
        let span = statement.span;
        let StatementKind::Declaration {
            names,
            initializer,
            destructure,
            ..
        } = statement.kind
        else {
            return Err(self.error_at("Invalid config statement", span));
        };
        let message = "Variable declarations cannot be mixed with config statements";
        self.legacy.push(Diagnostic::new(message, span));
        Ok(ConfigStatement::Declaration {
            names: names.into_iter().map(|typed| typed.name).collect(),
            value: initializer,
            destructure,
            span,
        })
    }

    /// A statement that config syntax does not have, like `System.err.println('...')`, which
    /// starts at token `position`. Where it does not parse as a script statement either, the
    /// strict error stands.
    fn legacy_statement(&mut self, position: usize) -> Parse<ConfigStatement> {
        let failure = self.position;
        self.position = position;
        let statement = self.speculate(Self::statement);
        let ends = self.at_separator() || self.at_list_end(true);
        if let Some(statement) = statement.filter(|_| ends) {
            let (end, errors) = (self.position, self.errors.len());
            self.position = failure;
            self.unexpected();
            self.legacy.extend(self.errors.split_off(errors));
            self.position = end;
            return Ok(ConfigStatement::Statement(statement));
        }
        self.position = failure;
        Err(self.unexpected())
    }
}
