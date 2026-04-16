//! Parses expressions, call arguments and closure parameters.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::call::append_closure;
use super::{Parse, Parser, is_keyword, is_name};
use crate::ast::{
    Access, AssignOperator, BinaryOperator, Block, Closure, Expression, ExpressionKind, Literal,
    Name, Parameter, Statement, StatementKind, Type, UnaryOperator,
};
use crate::token::{Dialect, TokenKind};

/// Binding strength of operators, weakest first, following the levels of `ScriptParser.g4`.
/// Each alternative there is its own level, so `as` binds tighter than `instanceof` and `<`.
const TERNARY: u8 = 1;
const OR: u8 = 2;
const AND: u8 = 3;
const BIT_OR: u8 = 4;
const XOR: u8 = 5;
const BIT_AND: u8 = 6;
const REGEX: u8 = 7;
const EQUALITY: u8 = 8;
const RELATIONAL: u8 = 9;
const INSTANCE_OF: u8 = 10;
const CAST: u8 = 11;
const SHIFT: u8 = 12;
const ADDITIVE: u8 = 13;
const MULTIPLICATIVE: u8 = 14;
const SIGN: u8 = 15;
const POWER: u8 = 16;
const NEGATION: u8 = 17;

enum Infix {
    Binary(BinaryOperator),
    Cast,
    InstanceOf { negated: bool },
    Ternary,
    Elvis,
}

struct Operator {
    infix: Infix,
    precedence: u8,
    /// Shift operators are written as two or three angle bracket tokens.
    tokens: usize,
    /// Whether the grammar allows a line break before the operator.
    after_newline: bool,
}

impl Parser<'_> {
    /// The `expression` rule.
    pub(super) fn expression(&mut self) -> Parse<Expression> {
        self.binary(0)
    }

    fn binary(&mut self, minimum: u8) -> Parse<Expression> {
        let mut left = self.unary()?;
        loop {
            let newlines = self.newline_count();
            let Some(operator) = self.operator_at(newlines) else {
                // Legacy syntax: inside brackets, an assignment is a value, as in `f(x = 1)`.
                if minimum == 0 && self.brackets > 0 && newlines == 0 {
                    return self.assignment_chain(left);
                }
                break;
            };
            if operator.precedence < minimum {
                break;
            }
            if newlines > 0 && !operator.after_newline {
                // Legacy syntax: inside brackets a line break does not end the expression.
                if self.brackets == 0 {
                    break;
                }
                let token = self.tokens[self.position + newlines];
                let text = self.text(token);
                self.legacy_at(format!("Unexpected input: '{text}'"), token.span);
            }
            // `as` and `instanceof` may name an argument, as in the call `foo as: 1`.
            let named = matches!(operator.infix, Infix::Cast | Infix::InstanceOf { .. });
            if named && self.nth(newlines + 1) == TokenKind::Colon {
                break;
            }
            let before = self.position;
            self.position += newlines;
            for _ in 0..operator.tokens {
                self.bump();
            }
            self.skip_newlines();
            let start = left.span;
            let kind = match operator.infix {
                Infix::Binary(BinaryOperator::In) => {
                    // `in` is also a name, so without an operand after it, it starts the
                    // arguments of a call like `foo in: 1` or `foo in, x`.
                    let minimum = operator.precedence + 1;
                    let Some(right) = self.speculate(|parser| parser.binary(minimum)) else {
                        self.position = before;
                        break;
                    };
                    ExpressionKind::Binary {
                        operator: BinaryOperator::In,
                        left: Box::new(left),
                        right: Box::new(right),
                    }
                }
                Infix::Binary(binary) => ExpressionKind::Binary {
                    operator: binary,
                    left: Box::new(left),
                    right: Box::new(self.binary(operator.precedence + 1)?),
                },
                Infix::Cast => ExpressionKind::Cast {
                    operand: Box::new(left),
                    target_type: self.operand_type()?,
                },
                Infix::InstanceOf { negated } => ExpressionKind::InstanceOf {
                    operand: Box::new(left),
                    target_type: self.operand_type()?,
                    negated,
                },
                Infix::Ternary => {
                    let mut then = self.expression()?;
                    // Legacy syntax: Groovy reads `c ? a = b : d` as `c ? (a = b) : d`.
                    if self.at(TokenKind::Assign) {
                        let assign = self.bump().span;
                        let message = "Expected ':' of the conditional expression";
                        self.legacy.push(crate::Diagnostic::new(message, assign));
                        let value = self.expression()?;
                        then = Expression {
                            span: then.span.to(value.span),
                            kind: ExpressionKind::Assign {
                                target: Box::new(then),
                                operator: AssignOperator::Assign,
                                value: Box::new(value),
                            },
                        };
                    }
                    self.skip_newlines();
                    self.expect(TokenKind::Colon, "':' of the conditional expression")?;
                    self.skip_newlines();
                    ExpressionKind::Ternary {
                        condition: Box::new(left),
                        then: Box::new(then),
                        otherwise: Box::new(self.binary(TERNARY)?),
                    }
                }
                Infix::Elvis => ExpressionKind::Elvis {
                    value: Box::new(left),
                    fallback: Box::new(self.binary(TERNARY)?),
                },
            };
            left = Expression {
                kind,
                span: self.span_from(start),
            };
        }
        Ok(left)
    }

    /// The type after `as` or `instanceof`. A `?` after it starts a conditional where one can
    /// follow, even on the next line, and else marks a nullable type, as in `x as Map? + y`.
    fn operand_type(&mut self) -> Parse<Type> {
        let mut target = self.type_annotation()?;
        let conditional = |parser: &mut Self| {
            parser.expect(TokenKind::Question, "'?'")?;
            parser.skip_newlines();
            parser.expression()?;
            parser.skip_newlines();
            parser.expect(TokenKind::Colon, "':'").map(|_| ())
        };
        if target.nullable {
            // Back to the `?` that the type took as its marker.
            self.position -= 1;
        }
        let question = self.dialect == Dialect::Script && self.at(TokenKind::Question);
        target.nullable = question && !self.lookahead(conditional);
        if target.nullable {
            self.bump();
        }
        target.span = self.span_from(target.span);
        Ok(target)
    }

    /// Whether the token `n` places ahead continues an expression on a new line: a property
    /// access, or an operator that cannot start a statement as `+`, `-` and the name `in` can.
    pub(super) fn continues_at(&self, n: usize) -> bool {
        let kind = self.nth(n);
        let access = matches!(
            kind,
            TokenKind::Dot | TokenKind::SafeDot | TokenKind::SpreadDot
        );
        let starts = matches!(kind, TokenKind::Add | TokenKind::Subtract | TokenKind::In);
        access || (self.operator_at(n).is_some() && !starts)
    }

    fn operator_at(&self, n: usize) -> Option<Operator> {
        use BinaryOperator as B;
        use TokenKind::*;
        let binary = |operator, precedence, after_newline| Operator {
            infix: Infix::Binary(operator),
            precedence,
            tokens: 1,
            after_newline,
        };
        let special = |infix, precedence| Operator {
            infix,
            precedence,
            tokens: 1,
            after_newline: true,
        };
        let shift = |operator, tokens| Operator {
            infix: Infix::Binary(operator),
            precedence: SHIFT,
            tokens,
            after_newline: true,
        };
        Some(match self.nth(n) {
            Power => binary(B::Power, POWER, false),
            Multiply => binary(B::Multiply, MULTIPLICATIVE, true),
            Divide => binary(B::Divide, MULTIPLICATIVE, true),
            Modulo => binary(B::Modulo, MULTIPLICATIVE, true),
            Add => binary(B::Add, ADDITIVE, false),
            Subtract => binary(B::Subtract, ADDITIVE, false),
            Less if self.nth(n + 1) == Less => shift(B::LeftShift, 2),
            Greater if self.nth(n + 1) == Greater && self.nth(n + 2) == Greater => {
                shift(B::UnsignedShift, 3)
            }
            Greater if self.nth(n + 1) == Greater => shift(B::RightShift, 2),
            RangeInclusive => binary(B::RangeInclusive, SHIFT, true),
            RangeExclusiveRight => binary(B::RangeExclusiveRight, SHIFT, true),
            As => special(Infix::Cast, CAST),
            Instanceof => special(Infix::InstanceOf { negated: false }, INSTANCE_OF),
            NotInstanceof => special(Infix::InstanceOf { negated: true }, INSTANCE_OF),
            Less => binary(B::Less, RELATIONAL, true),
            LessEqual => binary(B::LessEqual, RELATIONAL, true),
            Greater => binary(B::Greater, RELATIONAL, true),
            GreaterEqual => binary(B::GreaterEqual, RELATIONAL, true),
            In => binary(B::In, RELATIONAL, true),
            NotIn => binary(B::NotIn, RELATIONAL, true),
            Equal => binary(B::Equal, EQUALITY, true),
            NotEqual => binary(B::NotEqual, EQUALITY, true),
            Spaceship => binary(B::Spaceship, EQUALITY, true),
            RegexFind => binary(B::RegexFind, REGEX, true),
            RegexMatch => binary(B::RegexMatch, REGEX, true),
            BitAnd => binary(B::BitAnd, BIT_AND, true),
            Xor => binary(B::Xor, XOR, true),
            BitOr => binary(B::BitOr, BIT_OR, true),
            And => binary(B::And, AND, true),
            Or => binary(B::Or, OR, true),
            Question => special(Infix::Ternary, TERNARY),
            Elvis => special(Infix::Elvis, TERNARY),
            _ => return None,
        })
    }

    /// Prefix operators bind their operand at their own level, as ANTLR does for prefix rules.
    pub(super) fn unary(&mut self) -> Parse<Expression> {
        let start = self.current().span;
        let (operator, precedence) = match self.peek() {
            TokenKind::Not => (UnaryOperator::Not, NEGATION),
            TokenKind::BitNot => (UnaryOperator::BitNot, NEGATION),
            TokenKind::Add => (UnaryOperator::Plus, SIGN),
            TokenKind::Subtract => (UnaryOperator::Minus, SIGN),
            _ => return self.postfix(),
        };
        self.bump();
        if precedence == NEGATION {
            self.skip_newlines();
        }
        let operand = Box::new(self.binary(precedence)?);
        Ok(Expression {
            kind: ExpressionKind::Unary { operator, operand },
            span: self.span_from(start),
        })
    }

    /// A primary expression followed by property accesses, calls, closures and indexes.
    fn postfix(&mut self) -> Parse<Expression> {
        Ok(self.path(usize::MAX)?.0)
    }

    /// A primary expression and at most `limit` of the elements after it, with their number.
    pub(super) fn path(&mut self, limit: usize) -> Parse<(Expression, usize)> {
        let mut expression = self.primary()?;
        let mut count = 0;
        while count < limit {
            let start = expression.span;
            let kind = match self.peek() {
                TokenKind::Newline
                    if matches!(
                        self.peek_past_newlines(),
                        TokenKind::Dot | TokenKind::SafeDot | TokenKind::SpreadDot
                    ) =>
                {
                    self.skip_newlines();
                    continue;
                }
                TokenKind::Dot if matches!(self.nth(1), TokenKind::GStringBegin { .. }) => {
                    // Legacy syntax: `target."$name"` computes the name of the property.
                    self.bump();
                    let name = self.primary()?;
                    self.legacy_at("Expected a property name", name.span);
                    ExpressionKind::DynamicProperty {
                        target: Box::new(expression),
                        name: Box::new(name),
                    }
                }
                TokenKind::Dot | TokenKind::SafeDot | TokenKind::SpreadDot => {
                    let access = match self.bump().kind {
                        TokenKind::SafeDot => Access::Safe,
                        TokenKind::SpreadDot => Access::Spread,
                        _ if self.eat(TokenKind::At) => Access::Attribute,
                        _ => Access::Normal,
                    };
                    // Legacy syntax: Groovy allows a line break after the dot.
                    if self.at(TokenKind::Newline) {
                        let span = self.current().span;
                        let message = "Expected a property name";
                        self.legacy.push(crate::Diagnostic::new(message, span));
                        self.skip_newlines();
                    }
                    ExpressionKind::Property {
                        target: Box::new(expression),
                        name: self.property_name()?,
                        access,
                    }
                }
                TokenKind::LeftParen => {
                    let arguments = self.arguments()?;
                    expression = self.call(expression, arguments, true);
                    expression.span = self.span_from(start);
                    count += 1;
                    continue;
                }
                TokenKind::LeftBrace => {
                    // The official config builder rejects labels in any closure.
                    let closure = self.closure(self.dialect == Dialect::Script)?;
                    expression = append_closure(expression, closure);
                    expression.span = self.span_from(start);
                    count += 1;
                    continue;
                }
                TokenKind::LeftBracket => {
                    // After a name, `[` that starts no index starts the list or map argument of
                    // a call without parentheses, as in `foo [k: 1]`.
                    let named = matches!(
                        expression.kind,
                        ExpressionKind::Variable(_) | ExpressionKind::Property { .. }
                    );
                    let index = if named {
                        match self.speculate(Self::index_list) {
                            Some(index) => index,
                            None => return Ok((expression, count)),
                        }
                    } else {
                        self.index_list()?
                    };
                    ExpressionKind::Index {
                        target: Box::new(expression),
                        index,
                    }
                }
                TokenKind::Add | TokenKind::Subtract => match self.postfix_step(expression) {
                    Ok(step) => {
                        expression = step;
                        count += 1;
                        continue;
                    }
                    Err(expression) => return Ok((expression, count)),
                },
                _ => return Ok((expression, count)),
            };
            expression = Expression {
                kind,
                span: self.span_from(start),
            };
            count += 1;
        }
        Ok((expression, count))
    }

    /// The `indexPropertyArgs` rule: `[`, expressions, `]`.
    fn index_list(&mut self) -> Parse<Vec<Expression>> {
        self.expect(TokenKind::LeftBracket, "'['")?;
        let index = self.in_brackets(Self::expression_list)?;
        self.expect(TokenKind::RightBracket, "']'")?;
        Ok(index)
    }

    pub(super) fn primary(&mut self) -> Parse<Expression> {
        let token = self.current();
        let kind = match token.kind {
            kind if is_name(kind) && self.nth(1) == TokenKind::Arrow => self.lambda()?,
            kind if is_name(kind) || (self.groovy && kind == TokenKind::Include) => {
                self.bump();
                let text = self.text(token);
                self.check_name(text, token.span);
                ExpressionKind::Variable(text.to_owned())
            }
            TokenKind::IntegerLiteral => {
                self.bump();
                ExpressionKind::Literal(Literal::Integer(self.text(token).to_owned()))
            }
            TokenKind::FloatingPointLiteral => {
                self.bump();
                ExpressionKind::Literal(Literal::Float(self.text(token).to_owned()))
            }
            TokenKind::StringLiteral => {
                self.bump();
                ExpressionKind::Literal(Literal::String(self.string_value(token)))
            }
            TokenKind::BooleanLiteral => {
                self.bump();
                ExpressionKind::Literal(Literal::Boolean(self.text(token) == "true"))
            }
            TokenKind::NullLiteral => {
                self.bump();
                ExpressionKind::Literal(Literal::Null)
            }
            TokenKind::PrimitiveType => {
                self.bump();
                ExpressionKind::PrimitiveType(self.text(token).to_owned())
            }
            TokenKind::GStringBegin { .. } => self.gstring()?,
            TokenKind::New => {
                self.bump();
                let class = self.type_annotation()?;
                let arguments = self.arguments()?;
                ExpressionKind::New { class, arguments }
            }
            TokenKind::LeftParen if self.at_soft_cast() => return self.soft_cast(),
            TokenKind::LeftParen => {
                self.bump();
                let inner = self.in_brackets(|parser| {
                    parser.skip_newlines();
                    let inner = parser.expression()?;
                    parser.skip_newlines();
                    Ok(inner)
                })?;
                self.expect(TokenKind::RightParen, "')'")?;
                ExpressionKind::Parenthesized(Box::new(inner))
            }
            TokenKind::LeftBrace => ExpressionKind::Closure(Box::new(self.closure(false)?)),
            TokenKind::LeftBracket => self.list_or_map()?,
            _ => return Err(self.error_here("Expected an expression")),
        };
        Ok(Expression {
            kind,
            span: self.span_from(token.span),
        })
    }

    /// Legacy syntax: a Java lambda like `gene -> gene.trim()`, which Groovy reads as a closure
    /// with one parameter.
    fn lambda(&mut self) -> Parse<ExpressionKind> {
        let name = self.variable_name()?;
        let arrow = self.bump().span;
        self.legacy
            .push(crate::Diagnostic::new("Unexpected input: '->'", arrow));
        self.skip_newlines();
        let body = if self.at(TokenKind::LeftBrace) {
            self.block()?
        } else {
            let value = self.expression()?;
            let span = value.span;
            Block {
                statements: vec![Statement {
                    kind: StatementKind::Expression(value),
                    span,
                }],
                span,
            }
        };
        let parameter = Parameter {
            name,
            annotation: None,
            default: None,
        };
        Ok(ExpressionKind::Closure(Box::new(Closure {
            parameters: Some(vec![parameter]),
            body,
        })))
    }

    /// The `expressionList` rule: expressions separated by commas that may sit between newlines.
    pub(super) fn expression_list(&mut self) -> Parse<Vec<Expression>> {
        let mut expressions = vec![self.expression()?];
        while self.next_list_item() {
            expressions.push(self.expression()?);
        }
        Ok(expressions)
    }

    /// Moves past `nls , nls` when another item follows, but not past a trailing comma.
    pub(super) fn next_list_item(&mut self) -> bool {
        let newlines = self.newline_count();
        if self.nth(newlines) != TokenKind::Comma {
            return false;
        }
        let mut after = newlines + 1;
        while self.nth(after) == TokenKind::Newline {
            after += 1;
        }
        if matches!(
            self.nth(after),
            TokenKind::RightBracket | TokenKind::RightParen
        ) {
            return false;
        }
        self.position += after;
        true
    }

    /// The `namedProperty` rule: a name, keyword or string after a dot or before a colon.
    pub(super) fn property_name(&mut self) -> Parse<Name> {
        let token = self.current();
        if self.at_name() || is_keyword(token.kind) {
            self.bump();
            return Ok(Name {
                text: self.text(token).to_owned(),
                span: token.span,
            });
        }
        if token.kind == TokenKind::StringLiteral {
            self.bump();
            return Ok(Name {
                text: self.string_value(token),
                span: token.span,
            });
        }
        Err(self.error_here("Expected a property name"))
    }
}

#[cfg(test)]
#[path = "expression_tests.rs"]
pub(super) mod tests;
