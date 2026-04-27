//! Parses statements and expressions that only the legacy parser accepts: `for` and `while`
//! loops, `switch`, `break`, `continue`, `x++`, `*items` arguments, and line breaks inside
//! brackets.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::{Failed, Parse, Parser};
use crate::ast::{Expression, ExpressionKind, Name, StatementKind, SwitchCase};
use crate::token::TokenKind;

impl Parser<'_> {
    /// Whether a legacy loop, `break` or `continue` starts here.
    pub(super) fn at_legacy_loop(&self) -> bool {
        if self.peek() != TokenKind::Identifier {
            return false;
        }
        match self.text(self.current()) {
            "for" | "while" => self.nth(1) == TokenKind::LeftParen,
            "break" | "continue" => matches!(
                self.nth(1),
                TokenKind::Newline
                    | TokenKind::Semicolon
                    | TokenKind::RightBrace
                    | TokenKind::EndOfFile
            ),
            _ => false,
        }
    }

    /// A `for` or `while` loop, `break` or `continue`, with the error of strict syntax.
    pub(super) fn legacy_loop(&mut self) -> Parse<StatementKind> {
        let token = self.bump();
        let word = self.text(token);
        let message = match word {
            "for" | "while" => format!("`{word}` loops are no longer supported"),
            _ => format!("`{word}` is not allowed as an identifier because it is a Groovy keyword"),
        };
        self.legacy_at(message, token.span);
        match word {
            "break" => Ok(StatementKind::Break),
            "continue" => Ok(StatementKind::Continue),
            "while" => {
                self.expect(TokenKind::LeftParen, "'(' after 'while'")?;
                let condition = self.in_brackets(|parser| {
                    parser.skip_newlines();
                    let condition = parser.expression()?;
                    parser.skip_newlines();
                    Ok(condition)
                })?;
                self.expect(TokenKind::RightParen, "')'")?;
                self.skip_newlines();
                let body = self.statement_or_block()?;
                Ok(StatementKind::While { condition, body })
            }
            _ => self.for_statement(),
        }
    }

    /// Whether a `switch` statement starts here.
    pub(super) fn at_switch(&self) -> bool {
        self.peek() == TokenKind::Identifier
            && self.word(0) == "switch"
            && self.nth(1) == TokenKind::LeftParen
    }

    /// `switch (subject) { case value: statements default: statements }`, with the error of
    /// strict syntax. Consecutive labels share their statements.
    pub(super) fn switch_statement(&mut self) -> Parse<StatementKind> {
        let token = self.bump();
        self.legacy_at("`switch` statements are no longer supported", token.span);
        self.expect(TokenKind::LeftParen, "'(' after 'switch'")?;
        let subject = self.in_brackets(|parser| {
            parser.skip_newlines();
            let subject = parser.expression()?;
            parser.skip_newlines();
            Ok(subject)
        })?;
        self.expect(TokenKind::RightParen, "')'")?;
        self.skip_newlines();
        self.expect(TokenKind::LeftBrace, "'{'")?;
        let cases = self.in_braces(|parser| {
            let mut cases = Vec::new();
            loop {
                parser.skip_separators();
                if parser.at(TokenKind::RightBrace) {
                    return Ok(cases);
                }
                cases.push(parser.switch_case()?);
            }
        })?;
        self.expect(TokenKind::RightBrace, "'}'")?;
        if let Some((_, earlier)) = cases.split_last()
            && let Some(early) = earlier.iter().find(|case| case.default)
        {
            let message = "a default branch must only appear as the last branch of a switch";
            self.error_at(message, early.span);
        }
        Ok(StatementKind::Switch { subject, cases })
    }

    /// One or more `case value:` or `default:` labels and the statements after them.
    fn switch_case(&mut self) -> Parse<SwitchCase> {
        let start = self.current().span;
        let mut values = Vec::new();
        let mut default = false;
        if !self.at_case_label() {
            return Err(self.error_here("Expected 'case' or 'default'"));
        }
        while self.at_case_label() {
            if self.bump_word() == "default" {
                default = true;
            } else {
                values.push(self.in_brackets(Self::expression)?);
            }
            self.expect(TokenKind::Colon, "':'")?;
            self.skip_separators();
        }
        let body = self.statement_sequence(Self::at_case_label)?;
        Ok(SwitchCase {
            values,
            default,
            body,
            span: self.span_from(start),
        })
    }

    fn at_case_label(&self) -> bool {
        self.peek() == TokenKind::Identifier
            && match self.word(0) {
                "case" => true,
                "default" => self.nth(1) == TokenKind::Colon,
                _ => false,
            }
    }

    /// Consumes a token and gives its text.
    fn bump_word(&mut self) -> &str {
        let token = self.bump();
        self.text(token)
    }

    /// The rest of `for (x in items) body` or `for (init; condition; update) body`.
    fn for_statement(&mut self) -> Parse<StatementKind> {
        self.expect(TokenKind::LeftParen, "'(' after 'for'")?;
        self.skip_newlines();
        if let Some(variable) = self.speculate(Self::for_variable) {
            let items = self.in_brackets(Self::expression)?;
            self.skip_newlines();
            self.expect(TokenKind::RightParen, "')'")?;
            self.skip_newlines();
            let body = self.statement_or_block()?;
            return Ok(StatementKind::ForIn {
                variable,
                items,
                body,
            });
        }
        let init = if self.at(TokenKind::Semicolon) {
            None
        } else {
            Some(Box::new(self.statement()?))
        };
        self.expect(TokenKind::Semicolon, "';'")?;
        let condition = if self.at(TokenKind::Semicolon) {
            None
        } else {
            Some(self.in_brackets(Self::expression)?)
        };
        self.expect(TokenKind::Semicolon, "';'")?;
        let mut update = Vec::new();
        while !self.at(TokenKind::RightParen) {
            update.push(self.statement()?);
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RightParen, "')'")?;
        self.skip_newlines();
        let body = self.statement_or_block()?;
        Ok(StatementKind::For {
            init,
            condition,
            update,
            body,
        })
    }

    /// The variable of `for (x in items)`, `for (def x in items)`, `for (Type x in items)` or
    /// `for (Type x : items)`, up to and with `in` or `:`.
    fn for_variable(&mut self) -> Parse<Name> {
        self.eat(TokenKind::Def);
        if !(self.at_name() && matches!(self.nth(1), TokenKind::In | TokenKind::Colon)) {
            self.legacy_type()?;
        }
        let name = self.variable_name()?;
        if !(self.eat(TokenKind::In) || self.eat(TokenKind::Colon)) {
            return Err(Failed);
        }
        self.skip_newlines();
        Ok(name)
    }

    /// `x++` or `x--` after the operand `expression`: two touching `+` or `-` that no operand
    /// follows, which strict syntax cannot read.
    pub(super) fn postfix_step(
        &mut self,
        expression: Expression,
    ) -> Result<Expression, Expression> {
        let increment = match self.peek() {
            TokenKind::Add => true,
            TokenKind::Subtract => false,
            _ => return Err(expression),
        };
        let (first, second) = (self.current(), self.token_at(1));
        let touching = second.kind == first.kind && second.span.start == first.span.end;
        if !touching || starts_operand(self.nth(2)) {
            return Err(expression);
        }
        self.bump();
        self.bump();
        let text = if increment { "+" } else { "-" };
        self.legacy_at(format!("Unexpected input: '{text}'"), second.span);
        let span = expression.span.to(second.span);
        Ok(Expression {
            kind: ExpressionKind::Postfix {
                operand: Box::new(expression),
                increment,
            },
            span,
        })
    }

    /// The assignment that `target` starts, if an assignment operator follows it: legacy code
    /// uses an assignment as a value, as in `a = b = c` or `while (line = reader.readLine())`.
    pub(super) fn assignment_chain(&mut self, target: Expression) -> Parse<Expression> {
        let Some(operator) = super::statement::assign_operator(self.peek()) else {
            return Ok(target);
        };
        let assignable = matches!(
            target.kind,
            ExpressionKind::Variable(_)
                | ExpressionKind::Property { .. }
                | ExpressionKind::Index { .. }
        );
        if !assignable {
            return Ok(target);
        }
        let token = self.bump();
        let text = self.text(token).to_owned();
        self.legacy_at(format!("Unexpected input: '{text}'"), token.span);
        self.skip_newlines();
        let value = self.expression()?;
        let value = self.assignment_chain(value)?;
        let span = target.span.to(value.span);
        Ok(Expression {
            kind: ExpressionKind::Assign {
                target: Box::new(target),
                operator,
                value: Box::new(value),
            },
            span,
        })
    }

    /// Whether a legacy cast like `(Map) value` starts here: a type in parentheses that an
    /// operand follows.
    pub(super) fn at_soft_cast(&mut self) -> bool {
        // A primitive type like `(char)` can only start a cast, even of `(x)` or `-1`.
        let primitive = self.nth(1) == TokenKind::PrimitiveType;
        let class_like = primitive
            || self.nth(1) == TokenKind::CapitalizedIdentifier
            || (self.nth(1) == TokenKind::Identifier && self.nth(2) == TokenKind::Dot);
        class_like
            && self.lookahead(|parser| {
                parser.bump();
                parser.legacy_type()?;
                parser.expect(TokenKind::RightParen, "')'")?;
                let operand = starts_operand(parser.peek())
                    && (primitive
                        || !matches!(
                            parser.peek(),
                            TokenKind::Add
                                | TokenKind::Subtract
                                | TokenKind::LeftParen
                                | TokenKind::LeftBracket
                                | TokenKind::LeftBrace
                        ));
                if operand { Ok(()) } else { Err(Failed) }
            })
    }

    /// `(Type) value`, which strict syntax rejects; it converts like `value as Type`.
    pub(super) fn soft_cast(&mut self) -> Parse<Expression> {
        let start = self.bump().span;
        let target_type = self.legacy_type()?;
        self.expect(TokenKind::RightParen, "')'")?;
        // Strict syntax reads `(char)(x)` as a call, which fails when it runs; the legacy
        // parser casts, as the code means.
        if !self.at(TokenKind::LeftParen) {
            self.legacy_at("Unexpected input: '('", start);
        }
        let operand = self.unary()?;
        Ok(Expression {
            span: self.span_from(start),
            kind: ExpressionKind::Cast {
                operand: Box::new(operand),
                target_type,
            },
        })
    }

    /// The token `n` places ahead.
    fn token_at(&self, n: usize) -> crate::token::Token {
        self.tokens[(self.position + n).min(self.tokens.len() - 1)]
    }

    /// Runs a rule inside parentheses or square brackets, where the legacy parser ignores
    /// line breaks.
    pub(super) fn in_brackets<T>(&mut self, rule: impl FnOnce(&mut Self) -> Parse<T>) -> Parse<T> {
        self.brackets += 1;
        let result = rule(self);
        self.brackets -= 1;
        result
    }

    /// Runs a rule inside braces, like a closure, where line breaks end statements again.
    pub(super) fn in_braces<T>(&mut self, rule: impl FnOnce(&mut Self) -> Parse<T>) -> Parse<T> {
        let outer = std::mem::replace(&mut self.brackets, 0);
        let result = rule(self);
        self.brackets = outer;
        result
    }
}

/// Whether a token can begin an operand, so that `+` before it is an operator.
fn starts_operand(kind: TokenKind) -> bool {
    use TokenKind::*;
    super::is_name(kind)
        || matches!(
            kind,
            StringLiteral
                | IntegerLiteral
                | FloatingPointLiteral
                | BooleanLiteral
                | NullLiteral
                | GStringBegin { .. }
                | LeftParen
                | LeftBracket
                | LeftBrace
                | Not
                | BitNot
                | Add
                | Subtract
                | New
                | PrimitiveType
        )
}
