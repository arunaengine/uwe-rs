//! Parses calls: arguments, calls without parentheses, closures and their parameters.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::{Failed, Parse, Parser, is_keyword, is_name};
use crate::ast::{
    Access, Argument, Block, Closure, Expression, ExpressionKind, Literal, Name, Parameter,
};
use crate::token::{Dialect, TokenKind};

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

    /// Legacy syntax: the rest of a Groovy command chain, like `into ch` of `file 'x' into ch` in
    /// the outputs of DSL1 processes, where a name and its arguments call that method on the
    /// result of the command before.
    pub(super) fn command_chain(&mut self, mut target: Expression) -> Parse<Expression> {
        while is_name(self.peek()) && self.chain_arguments_follow() {
            let token = self.bump();
            let name = Name {
                text: self.text(token).to_owned(),
                span: token.span,
            };
            self.legacy_at(format!("Unexpected input: '{}'", name.text), name.span);
            // `into (a, b)` gives the arguments in parentheses.
            let arguments = if self.at(TokenKind::LeftParen) {
                self.arguments()?
            } else {
                self.argument_list()?
            };
            let span = target.span;
            target = Expression {
                kind: ExpressionKind::MethodCall {
                    target: Some(Box::new(target)),
                    name,
                    access: Access::Normal,
                    arguments,
                    parentheses: false,
                },
                span: self.span_from(span),
            };
        }
        Ok(target)
    }

    /// Whether the token after the current name starts arguments on the same line.
    fn chain_arguments_follow(&mut self) -> bool {
        self.position += 1;
        let follows = self.at_argument_start() || self.at(TokenKind::LeftParen);
        self.position -= 1;
        follows
    }

    /// The `argumentList` rule: call arguments without parentheses.
    pub(super) fn argument_list(&mut self) -> Parse<Vec<Argument>> {
        let mut arguments = vec![self.command_argument()?];
        while self.peek_past_newlines() == TokenKind::Comma {
            self.skip_newlines();
            self.bump();
            self.skip_newlines();
            arguments.push(self.command_argument()?);
        }
        self.check_names(&arguments);
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

    /// `{ a, b -> ... }`, `{ -> ... }` or `{ ... }` with the implicit parameter `it`. As in
    /// the official grammar, only a closure after an expression may have `labels`.
    pub(super) fn closure(&mut self, labels: bool) -> Parse<Closure> {
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
        let statements = self.in_braces(|parser| parser.closure_body(labels))?;
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
        // Legacy syntax: `... name` takes the remaining arguments, as an array of objects.
        if self.at(TokenKind::RangeInclusive) && self.nth(1) == TokenKind::Dot {
            let start = self.bump().span;
            self.bump();
            self.legacy_at("Unexpected input: '...'", start);
            let name = self.variable_name()?;
            let span = start.to(name.span);
            let object = crate::ast::QualifiedName {
                parts: vec![Name {
                    text: "Object".into(),
                    span,
                }],
            };
            let annotation = Some(crate::ast::Type {
                name: object,
                arguments: Vec::new(),
                nullable: false,
                dimensions: 1,
                span,
            });
            return Ok(Parameter {
                name,
                annotation,
                default: None,
            });
        }
        let legacy = self.speculate(|parser| {
            let mut legacy = parser.legacy_type()?;
            // Legacy syntax: `Type... name` takes the remaining arguments, as an array.
            if parser.at(TokenKind::RangeInclusive) && parser.nth(1) == TokenKind::Dot {
                parser.bump();
                parser.bump();
                legacy.dimensions += 1;
            }
            if parser.at_name() {
                Ok(legacy)
            } else {
                Err(Failed)
            }
        });
        let name = self.variable_name()?;
        let annotation = match legacy {
            Some(legacy) => Some(legacy),
            // Config files have no `name: Type` parameters.
            None if self.dialect == Dialect::Script && self.eat(TokenKind::Colon) => {
                Some(self.type_annotation()?)
            }
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
        let arguments = self.in_brackets(|parser| {
            parser.skip_newlines();
            let mut arguments = Vec::new();
            if !parser.at(TokenKind::RightParen) {
                arguments.push(parser.argument()?);
                while parser.next_list_item() {
                    arguments.push(parser.argument()?);
                }
                parser.skip_newlines();
                parser.eat(TokenKind::Comma);
            }
            parser.skip_newlines();
            Ok(arguments)
        })?;
        self.expect(TokenKind::RightParen, "')'")?;
        self.check_names(&arguments);
        Ok(arguments)
    }

    /// Reports named arguments that repeat an earlier name, like `f(a: 1, 'a': 2)`.
    fn check_names(&mut self, arguments: &[Argument]) {
        let names: Vec<&Name> = arguments.iter().filter_map(|a| a.name.as_ref()).collect();
        for (index, name) in names.iter().enumerate() {
            if names[..index]
                .iter()
                .any(|earlier| earlier.text == name.text)
            {
                let message = format!("Duplicated named argument '{}' found", name.text);
                self.error_at(message, name.span);
            }
        }
    }

    /// An argument of a call without parentheses. Legacy syntax: it may be a command itself,
    /// like `log.info "x"` of `exit 1, log.info "x"`, which Groovy reads as `log.info("x")`.
    fn command_argument(&mut self) -> Parse<Argument> {
        let mut argument = self.argument()?;
        if matches!(argument.value.kind, ExpressionKind::Property { .. })
            && self.at_argument_start()
        {
            self.legacy_at(
                format!("Unexpected input: '{}'", self.text(self.current())),
                self.current().span,
            );
            let arguments = self.argument_list()?;
            argument.value = self.call(argument.value, arguments, false);
        }
        Ok(argument)
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
        // Legacy syntax: `*items` passes the elements of a list as the arguments.
        if name.is_none() && self.brackets > 0 && self.at(TokenKind::Multiply) {
            let start = self.bump().span;
            self.legacy_at("Unexpected input: '*'", start);
            let items = self.expression()?;
            let value = Expression {
                span: self.span_from(start),
                kind: ExpressionKind::Spread(Box::new(items)),
            };
            return Ok(Argument { name, value });
        }
        Ok(Argument {
            name,
            value: self.expression()?,
        })
    }

    /// Whether the next token can begin the arguments of a call without parentheses. A `[`
    /// right after the name is left over only where it could not start an index.
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
                    | LeftBracket
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
