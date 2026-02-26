//! Parses statements and blocks.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::{Failed, Parse, Parser};
use crate::ast::{
    Access, AssignOperator, Block, Catch, Expression, ExpressionKind, QualifiedName, Statement,
    StatementKind, TypedName,
};
use crate::token::TokenKind;

impl Parser<'_> {
    /// The `statement` rule.
    pub(super) fn statement(&mut self) -> Parse<Statement> {
        let start = self.current().span;
        let kind = match self.peek() {
            TokenKind::If => self.if_statement()?,
            TokenKind::Try => self.try_statement()?,
            TokenKind::Return => {
                self.bump();
                if self.at_statement_end() {
                    StatementKind::Return(None)
                } else {
                    StatementKind::Return(Some(self.expression()?))
                }
            }
            TokenKind::Throw => {
                self.bump();
                StatementKind::Throw(self.expression()?)
            }
            TokenKind::Assert => self.assert_statement()?,
            TokenKind::Semicolon => {
                self.bump();
                StatementKind::Empty
            }
            TokenKind::Def => self.declaration()?,
            _ => self.expression_statement()?,
        };
        Ok(Statement {
            kind,
            span: self.span_from(start),
        })
    }

    /// A braced block: `{` then statements then `}`.
    pub(super) fn block(&mut self) -> Parse<Block> {
        let start = self.expect(TokenKind::LeftBrace, "'{'")?.span;
        let statements = self.statement_sequence(|_| false)?;
        self.expect(TokenKind::RightBrace, "'}'")?;
        Ok(Block {
            statements,
            span: self.span_from(start),
        })
    }

    /// Statements separated by newlines or semicolons, up to a closing brace, the end of
    /// input, or a statement start where `is_end` holds, such as a section label.
    pub(super) fn statement_sequence(
        &mut self,
        is_end: impl Fn(&Self) -> bool,
    ) -> Parse<Vec<Statement>> {
        self.sequence(is_end, false)
    }

    /// The statements of a closure, which may carry labels as in `branch { big: it > 10 }`.
    pub(super) fn closure_body(&mut self, labels: bool) -> Parse<Vec<Statement>> {
        self.sequence(|_| false, labels)
    }

    fn sequence(&mut self, is_end: impl Fn(&Self) -> bool, labels: bool) -> Parse<Vec<Statement>> {
        let mut statements = Vec::new();
        loop {
            self.skip_separators();
            if matches!(self.peek(), TokenKind::RightBrace | TokenKind::EndOfFile) || is_end(self) {
                return Ok(statements);
            }
            let statement = if labels {
                self.labeled_statement()?
            } else {
                self.statement()?
            };
            statements.push(statement);
            if !self.at_statement_end() {
                return Err(self.error_here("Expected a new line or ';' after the statement"));
            }
        }
    }

    fn labeled_statement(&mut self) -> Parse<Statement> {
        if !(self.at_name() && self.nth(1) == TokenKind::Colon) {
            return self.statement();
        }
        let start = self.current().span;
        let label = self.name()?;
        self.bump();
        self.skip_newlines();
        let statement = Box::new(self.labeled_statement()?);
        Ok(Statement {
            kind: StatementKind::Labeled { label, statement },
            span: self.span_from(start),
        })
    }

    fn at_statement_end(&self) -> bool {
        matches!(
            self.peek(),
            TokenKind::Newline
                | TokenKind::Semicolon
                | TokenKind::RightBrace
                | TokenKind::EndOfFile
        )
    }

    fn if_statement(&mut self) -> Parse<StatementKind> {
        self.bump();
        self.expect(TokenKind::LeftParen, "'(' after 'if'")?;
        self.skip_newlines();
        let condition = self.expression()?;
        self.skip_newlines();
        self.expect(TokenKind::RightParen, "')'")?;
        self.skip_newlines();
        let then = self.statement_or_block()?;
        let otherwise = if self.peek_past_newlines() == TokenKind::Else {
            self.skip_newlines();
            self.bump();
            self.skip_newlines();
            Some(self.statement_or_block()?)
        } else {
            None
        };
        Ok(StatementKind::If {
            condition,
            then,
            otherwise,
        })
    }

    /// A block, or a single statement treated as a block of one. As in the official parser,
    /// braces only make a block if the branch can end there: `if (a) { b }.c()` has a closure.
    fn statement_or_block(&mut self) -> Parse<Block> {
        if self.at(TokenKind::LeftBrace) {
            let block = self.speculate(|parser| {
                let block = parser.block()?;
                let newlines = parser.newline_count();
                let ends = match newlines {
                    0 => matches!(
                        parser.peek(),
                        TokenKind::Semicolon
                            | TokenKind::RightBrace
                            | TokenKind::EndOfFile
                            | TokenKind::Else
                            | TokenKind::Catch
                    ),
                    _ => !parser.continues_at(newlines),
                };
                if ends { Ok(block) } else { Err(Failed) }
            });
            if let Some(block) = block {
                return Ok(block);
            }
        }
        let statement = self.statement()?;
        Ok(Block {
            span: statement.span,
            statements: vec![statement],
        })
    }

    fn try_statement(&mut self) -> Parse<StatementKind> {
        self.bump();
        self.skip_newlines();
        let body = self.statement_or_block()?;
        let mut catches = Vec::new();
        while self.peek_past_newlines() == TokenKind::Catch {
            self.skip_newlines();
            self.bump();
            self.expect(TokenKind::LeftParen, "'(' after 'catch'")?;
            let named_first =
                self.at_name() && matches!(self.nth(1), TokenKind::RightParen | TokenKind::Colon);
            let (name, types) = if named_first {
                let name = self.variable_name()?;
                let types = if self.eat(TokenKind::Colon) {
                    self.catch_types()?
                } else {
                    Vec::new()
                };
                (name, types)
            } else {
                let types = self.catch_types()?;
                (self.variable_name()?, types)
            };
            self.expect(TokenKind::RightParen, "')'")?;
            self.skip_newlines();
            let body = self.statement_or_block()?;
            catches.push(Catch { name, types, body });
        }
        Ok(StatementKind::Try { body, catches })
    }

    fn catch_types(&mut self) -> Parse<Vec<QualifiedName>> {
        let mut types = vec![self.qualified_class_name()?];
        while self.eat(TokenKind::BitOr) {
            types.push(self.qualified_class_name()?);
        }
        Ok(types)
    }

    fn assert_statement(&mut self) -> Parse<StatementKind> {
        self.bump();
        let condition = self.expression()?;
        let message = if self.peek_past_newlines() == TokenKind::Colon {
            self.skip_newlines();
            self.bump();
            self.skip_newlines();
            Some(self.expression()?)
        } else {
            None
        };
        Ok(StatementKind::Assert { condition, message })
    }

    /// `def x`, `def x: T = 1`, `def (a, b) = pair` or `def String x = ''`.
    fn declaration(&mut self) -> Parse<StatementKind> {
        self.bump();
        if self.at(TokenKind::LeftParen) {
            let names = self.destructure_names()?;
            let initializer = Some(
                self.initializer()?
                    .ok_or_else(|| self.error_here("Expected '=' after the variable names"))?,
            );
            return Ok(StatementKind::Declaration {
                names,
                legacy_type: None,
                initializer,
                destructure: true,
            });
        }
        let legacy_type = self.speculate(|parser| {
            let legacy = parser.legacy_type()?;
            if parser.at_name() {
                Ok(legacy)
            } else {
                Err(Failed)
            }
        });
        let name = self.variable_name()?;
        let annotation = if legacy_type.is_none() && self.eat(TokenKind::Colon) {
            Some(self.type_annotation()?)
        } else {
            None
        };
        Ok(StatementKind::Declaration {
            names: vec![TypedName { name, annotation }],
            legacy_type,
            initializer: self.initializer()?,
            destructure: false,
        })
    }

    /// `( nls a: A , nls b nls )` of a destructuring declaration.
    fn destructure_names(&mut self) -> Parse<Vec<TypedName>> {
        self.expect(TokenKind::LeftParen, "'('")?;
        self.skip_newlines();
        let mut names = Vec::new();
        loop {
            let name = self.variable_name()?;
            let annotation = if self.eat(TokenKind::Colon) {
                Some(self.type_annotation()?)
            } else {
                None
            };
            names.push(TypedName { name, annotation });
            if !self.eat(TokenKind::Comma) {
                break;
            }
            self.skip_newlines();
        }
        self.skip_newlines();
        self.expect(TokenKind::RightParen, "')'")?;
        Ok(names)
    }

    /// An optional `nls = nls expression`.
    fn initializer(&mut self) -> Parse<Option<Expression>> {
        if self.peek_past_newlines() != TokenKind::Assign {
            return Ok(None);
        }
        self.skip_newlines();
        self.bump();
        self.skip_newlines();
        Ok(Some(self.expression()?))
    }

    /// Typed declarations, multiple assignments, assignments and expression statements.
    fn expression_statement(&mut self) -> Parse<StatementKind> {
        let start = self.position;
        let typed = self.speculate(|parser| {
            let legacy = parser.legacy_type()?;
            if parser.at_name() {
                Ok(legacy)
            } else {
                Err(Failed)
            }
        });
        if let Some(legacy_type) = typed {
            let name = self.variable_name()?;
            return Ok(StatementKind::Declaration {
                names: vec![TypedName {
                    name,
                    annotation: None,
                }],
                legacy_type: Some(legacy_type),
                initializer: self.initializer()?,
                destructure: false,
            });
        }
        if self.at(TokenKind::LeftParen) {
            let names = self.speculate(|parser| {
                let names = parser.destructure_names()?;
                let plain = names.len() > 1 && names.iter().all(|n| n.annotation.is_none());
                if plain && parser.peek_past_newlines() == TokenKind::Assign {
                    Ok(names)
                } else {
                    Err(Failed)
                }
            });
            if let Some(names) = names {
                let value = self.initializer()?.ok_or(Failed)?;
                return Ok(StatementKind::MultipleAssign {
                    names: names.into_iter().map(|typed| typed.name).collect(),
                    value,
                });
            }
        }
        let target = self.expression()?;
        let newlines = self.newline_count();
        if let Some(operator) = assign_operator(self.nth(newlines)) {
            self.position += newlines;
            self.bump();
            self.skip_newlines();
            let value = self.expression()?;
            return Ok(StatementKind::Assign {
                target,
                operator,
                value,
            });
        }
        let expression = self.command_expression(target)?;
        if self.at(TokenKind::Comma)
            && let Some(command) = self.retry_command(start)
        {
            return Ok(StatementKind::Expression(command));
        }
        Ok(StatementKind::Expression(expression))
    }

    /// ANTLR reads `path("x"), emit: y` as `path` with the arguments `("x")` and `emit: y`,
    /// because the greedy reading cannot end the statement; this parses that reading.
    fn retry_command(&mut self, start: usize) -> Option<Expression> {
        let resume = self.position;
        self.position = start;
        let command = self.speculate(|parser| {
            let name = parser.name()?;
            let mut target = Expression {
                kind: ExpressionKind::Variable(name.text),
                span: name.span,
            };
            while parser.eat(TokenKind::Dot) {
                let property = parser.property_name()?;
                target = Expression {
                    span: target.span.to(property.span),
                    kind: ExpressionKind::Property {
                        target: Box::new(target),
                        name: property,
                        access: Access::Normal,
                    },
                };
            }
            let arguments = parser.argument_list()?;
            if !parser.at_statement_end() {
                return Err(Failed);
            }
            Ok(parser.call(target, arguments, false))
        });
        if command.is_none() {
            self.position = resume;
        }
        command
    }
}

fn assign_operator(kind: TokenKind) -> Option<AssignOperator> {
    use AssignOperator as A;
    use TokenKind::*;
    Some(match kind {
        Assign => A::Assign,
        AddAssign => A::Add,
        SubtractAssign => A::Subtract,
        MultiplyAssign => A::Multiply,
        DivideAssign => A::Divide,
        ModuloAssign => A::Modulo,
        PowerAssign => A::Power,
        BitAndAssign => A::BitAnd,
        BitOrAssign => A::BitOr,
        XorAssign => A::Xor,
        LeftShiftAssign => A::LeftShift,
        RightShiftAssign => A::RightShift,
        UnsignedShiftAssign => A::UnsignedShift,
        ElvisAssign => A::Elvis,
        _ => return None,
    })
}

#[cfg(test)]
#[path = "statement_tests.rs"]
mod tests;
