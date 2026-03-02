//! Lowering of expressions: names, literals, GStrings, closures, calls and operators.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_parser::Span;
use nextflow_parser::ast::{
    self, ExpressionKind as Syntax, GStringPart, Literal, MapKey, UnaryOperator,
};

use super::Lowering;
use super::names::{CLASSES, SCRIPT_CONSTANTS};
use crate::lowered::{Expression, ExpressionKind as Kind, Part, Unary};
use crate::number::Number;
use crate::value::Value;

impl Lowering<'_> {
    pub(super) fn expression(&mut self, expression: &ast::Expression) -> Expression {
        let span = expression.span;
        let kind = match &expression.kind {
            Syntax::Variable(name) => self.variable(name, span),
            Syntax::Literal(literal) => self.literal(literal, span),
            Syntax::GString(parts) => self.gstring(parts),
            Syntax::Parenthesized(inner) => return self.expression(inner),
            Syntax::List(items) => Kind::List(self.expressions(items)),
            Syntax::Map(entries) => Kind::Map(
                entries
                    .iter()
                    .map(|entry| {
                        let key = match &entry.key {
                            MapKey::Name(name) => literal_string(&name.text, name.span),
                            MapKey::Expression(key) => self.expression(key),
                        };
                        (key, self.expression(&entry.value))
                    })
                    .collect(),
            ),
            Syntax::Property {
                target,
                name,
                access,
            } => Kind::Property {
                target: Box::new(self.expression(target)),
                name: name.text.as_str().into(),
                access: *access,
            },
            Syntax::Index { target, index } => Kind::Index {
                target: Box::new(self.expression(target)),
                index: self.expressions(index),
            },
            Syntax::Unary { operator, operand } => Kind::Unary {
                operator: match operator {
                    UnaryOperator::Not => Unary::Not,
                    UnaryOperator::BitNot => Unary::BitwiseNot,
                    UnaryOperator::Plus => Unary::Positive,
                    UnaryOperator::Minus => Unary::Negate,
                },
                operand: Box::new(self.expression(operand)),
            },
            Syntax::Ternary {
                condition,
                then,
                otherwise,
            } => Kind::Ternary {
                condition: Box::new(self.expression(condition)),
                then: Box::new(self.expression(then)),
                otherwise: Box::new(self.expression(otherwise)),
            },
            Syntax::Elvis { value, fallback } => Kind::Elvis {
                value: Box::new(self.expression(value)),
                fallback: Box::new(self.expression(fallback)),
            },
            Syntax::Closure(_)
            | Syntax::MethodCall { .. }
            | Syntax::Call { .. }
            | Syntax::New { .. }
            | Syntax::Binary { .. }
            | Syntax::Cast { .. }
            | Syntax::InstanceOf { .. } => {
                self.error("This expression is not supported yet", span);
                Kind::Literal(Value::Null)
            }
            Syntax::PrimitiveType(name) => {
                self.error(format!("The type `{name}` cannot be used as a value"), span);
                Kind::Literal(Value::Null)
            }
        };
        Expression { kind, span }
    }

    fn expressions(&mut self, expressions: &[ast::Expression]) -> Vec<Expression> {
        expressions
            .iter()
            .map(|item| self.expression(item))
            .collect()
    }

    /// A name used as a value: a variable, a built-in constant, or an error.
    fn variable(&mut self, name: &str, span: Span) -> Kind {
        if let Some(place) = self.scopes.resolve(name) {
            return Kind::Variable(place);
        }
        if SCRIPT_CONSTANTS.contains(&name) {
            return Kind::Builtin(name.into());
        }
        if CLASSES.contains(&name) {
            self.error(
                format!("Using the class `{name}` as a value is not supported yet"),
                span,
            );
        } else {
            self.error(format!("`{name}` is not defined"), span);
        }
        Kind::Literal(Value::Null)
    }

    fn literal(&mut self, literal: &Literal, span: Span) -> Kind {
        let number = match literal {
            Literal::Integer(text) => Number::parse_integer(text),
            Literal::Float(text) => Number::parse_decimal(text),
            Literal::String(text) => return Kind::Literal(Value::string(text.as_str())),
            Literal::Boolean(value) => return Kind::Literal(Value::Boolean(*value)),
            Literal::Null => return Kind::Literal(Value::Null),
        };
        match number {
            Ok(number) => Kind::Literal(Value::Number(number)),
            Err(error) => {
                self.error(error.message, span);
                Kind::Literal(Value::Null)
            }
        }
    }

    /// A GString. Its values are lowered as nested, so a GString inside them stays a GString.
    fn gstring(&mut self, parts: &[GStringPart]) -> Kind {
        let keep_gstring = self.in_gstring;
        let outer = std::mem::replace(&mut self.in_gstring, true);
        let parts = parts
            .iter()
            .map(|part| match part {
                GStringPart::Text(text) => Part::Text(text.as_str().into()),
                GStringPart::Value(value) => Part::Value(self.expression(value)),
            })
            .collect();
        self.in_gstring = outer;
        Kind::Interpolate {
            parts,
            keep_gstring,
        }
    }
}

fn literal_string(text: &str, span: Span) -> Expression {
    Expression {
        kind: Kind::Literal(Value::string(text)),
        span,
    }
}
