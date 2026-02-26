//! Parses calls: arguments, calls without parentheses, closures and their parameters.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::{Failed, Parse, Parser, is_keyword, is_name};
use crate::ast::{
    Access, Argument, Block, Closure, Expression, ExpressionKind, Literal, Name, Parameter,
};
use crate::token::TokenKind;

impl Parser<'_> {
    /// Turns a statement expression into a call without parentheses, like `cpus 4` or
    /// `tuple val(x), path(y)`, where the grammar allows it; otherwise returns it unchanged.
    pub(super) fn command_expression(&mut self, target: Expression) -> Parse<Expression> {
        let directive = matches!(
            target.kind,
            ExpressionKind::Variable(_) | ExpressionKind::Property { .. }
        );
        if !directive || !self.at_argument_start() {
            return Ok(target);
        }
        let arguments = self.argument_list()?;
        Ok(self.call(target, arguments, false))
    }

    /// The `argumentList` rule: call arguments without parentheses.
    pub(super) fn argument_list(&mut self) -> Parse<Vec<Argument>> {
        let mut arguments = vec![self.argument()?];
        while self.peek_past_newlines() == TokenKind::Comma {
            self.skip_newlines();
            self.bump();
            self.skip_newlines();
            arguments.push(self.argument()?);
        }
        Ok(arguments)
    }

    /// The `formalParameterList` rule of closures and functions.
    pub(super) fn parameters(&mut self) -> Parse<Vec<Parameter>> {
        let mut parameters = vec![self.parameter()?];
        while self.eat(TokenKind::Comma) {
            self.skip_newlines();
            parameters.push(self.parameter()?);
        }
        Ok(parameters)
    }

    /// `{ a, b -> ... }`, `{ -> ... }` or `{ ... }` with the implicit parameter `it`.
    pub(super) fn closure(&mut self) -> Parse<Closure> {
        let start = self.expect(TokenKind::LeftBrace, "'{'")?.span;
        let parameters = self.speculate(|parser| {
            parser.skip_newlines();
            let parameters = if parser.at(TokenKind::Arrow) {
                Vec::new()
            } else {
                parser.parameters()?
            };
            parser.skip_newlines();
            parser.expect(TokenKind::Arrow, "'->'")?;
            Ok(parameters)
        });
        let statements = self.closure_body()?;
        self.expect(TokenKind::RightBrace, "'}' to close the closure")?;
        Ok(Closure {
            parameters,
            body: Block {
                statements,
                span: self.span_from(start),
            },
        })
    }

    fn parameter(&mut self) -> Parse<Parameter> {
        self.eat(TokenKind::Def);
        let legacy = self.speculate(|parser| {
            let legacy = parser.legacy_type()?;
            if parser.at_name() {
                Ok(legacy)
            } else {
                Err(Failed)
            }
        });
        let name = self.variable_name()?;
        let annotation = match legacy {
            Some(legacy) => Some(legacy),
            None if self.eat(TokenKind::Colon) => Some(self.type_annotation()?),
            None => None,
        };
        let default = if self.peek_past_newlines() == TokenKind::Assign {
            self.skip_newlines();
            self.bump();
            self.skip_newlines();
            Some(self.expression()?)
        } else {
            None
        };
        Ok(Parameter {
            name,
            annotation,
            default,
        })
    }

    /// Parenthesized call arguments.
    pub(super) fn arguments(&mut self) -> Parse<Vec<Argument>> {
        self.expect(TokenKind::LeftParen, "'('")?;
        self.skip_newlines();
        let mut arguments = Vec::new();
        if !self.at(TokenKind::RightParen) {
            arguments.push(self.argument()?);
            while self.next_list_item() {
                arguments.push(self.argument()?);
            }
            self.skip_newlines();
            self.eat(TokenKind::Comma);
        }
        self.skip_newlines();
        self.expect(TokenKind::RightParen, "')'")?;
        Ok(arguments)
    }

    /// An argument, which is named when it starts with a property name and a colon.
    fn argument(&mut self) -> Parse<Argument> {
        let named =
            (self.at_name() || is_keyword(self.peek()) || self.at(TokenKind::StringLiteral))
                && self.nth(1) == TokenKind::Colon;
        let name = if named {
            let name = self.property_name()?;
            self.bump();
            Some(name)
        } else {
            None
        };
        Ok(Argument {
            name,
            value: self.expression()?,
        })
    }

    /// Whether the next token can begin the arguments of a call without parentheses.
    fn at_argument_start(&self) -> bool {
        use TokenKind::*;
        let kind = self.peek();
        is_name(kind)
            || (is_keyword(kind) && self.nth(1) == Colon)
            || matches!(
                kind,
                StringLiteral
                    | IntegerLiteral
                    | FloatingPointLiteral
                    | BooleanLiteral
                    | NullLiteral
                    | GStringBegin { .. }
                    | New
                    | Not
                    | BitNot
                    | PrimitiveType
            )
    }

    /// Calls on names, properties and constant names like `'m'` or `int` become method calls;
    /// other callees are called as values.
    pub(super) fn call(
        &self,
        callee: Expression,
        arguments: Vec<Argument>,
        parentheses: bool,
    ) -> Expression {
        let span = callee.span;
        let kind = match callee.kind {
            ExpressionKind::Variable(text)
            | ExpressionKind::PrimitiveType(text)
            | ExpressionKind::Literal(Literal::String(text)) => ExpressionKind::MethodCall {
                target: None,
                name: Name { text, span },
                access: Access::Normal,
                arguments,
                parentheses,
            },
            ExpressionKind::Property {
                target,
                name,
                access,
            } => ExpressionKind::MethodCall {
                target: Some(target),
                name,
                access,
                arguments,
                parentheses,
            },
            kind => ExpressionKind::Call {
                callee: Box::new(Expression { kind, span }),
                arguments,
            },
        };
        Expression {
            kind,
            span: self.span_from(span),
        }
    }
}

/// A closure after a call becomes its last argument, as in `foo(x) { ... }`.
pub(super) fn append_closure(mut callee: Expression, closure: Closure) -> Expression {
    // Unlike an argument list, the official builder ignores parentheses here: `(a.b) { }`
    // calls the method `b` of `a`, and `(foo(x)) { }` adds the closure to that call.
    while let ExpressionKind::Parenthesized(inner) = callee.kind {
        callee = *inner;
    }
    let argument = Argument {
        name: None,
        value: Expression {
            span: closure.body.span,
            kind: ExpressionKind::Closure(Box::new(closure)),
        },
    };
    let span = callee.span;
    let kind = match callee.kind {
        ExpressionKind::MethodCall {
            target,
            name,
            access,
            mut arguments,
            parentheses,
        } => {
            arguments.push(argument);
            ExpressionKind::MethodCall {
                target,
                name,
                access,
                arguments,
                parentheses,
            }
        }
        ExpressionKind::Call {
            callee,
            mut arguments,
        } => {
            arguments.push(argument);
            ExpressionKind::Call { callee, arguments }
        }
        ExpressionKind::Variable(text)
        | ExpressionKind::PrimitiveType(text)
        | ExpressionKind::Literal(Literal::String(text)) => ExpressionKind::MethodCall {
            target: None,
            name: Name { text, span },
            access: Access::Normal,
            arguments: vec![argument],
            parentheses: false,
        },
        ExpressionKind::Property {
            target,
            name,
            access,
        } => ExpressionKind::MethodCall {
            target: Some(target),
            name,
            access,
            arguments: vec![argument],
            parentheses: false,
        },
        kind => ExpressionKind::Call {
            callee: Box::new(Expression { kind, span }),
            arguments: vec![argument],
        },
    };
    Expression { kind, span }
}
