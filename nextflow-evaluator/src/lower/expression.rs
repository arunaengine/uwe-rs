//! Lowering of expressions: names, literals, GStrings, closures, calls and operators.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_parser::Span;
use nextflow_parser::ast::{
    self, BinaryOperator as B, ExpressionKind as Syntax, GStringPart, Literal, MapKey,
    UnaryOperator,
};

use super::Lowering;
use super::names::{CLASSES, SCRIPT_CONSTANTS, SCRIPT_FUNCTIONS};
use crate::lowered::{
    Arguments, Code, Expression, ExpressionKind as Kind, Function, Parameter, Part, Unary,
};
use crate::number::Number;
use crate::value::{Operator, Relation, Value};

impl Lowering<'_> {
    pub(super) fn expression(&mut self, expression: &ast::Expression) -> Expression {
        let span = expression.span;
        let kind = match &expression.kind {
            Syntax::Variable(name) => self.variable(name, span),
            Syntax::Literal(literal) => self.literal(literal, span),
            Syntax::GString(parts) => self.gstring(parts),
            Syntax::Parenthesized(inner) => return self.expression(inner),
            Syntax::Closure(closure) => self.closure(closure, span),
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
            } => match self.class_name(target) {
                Some(class) => Kind::StaticProperty {
                    class,
                    name: name.text.as_str().into(),
                },
                None => Kind::Property {
                    target: Box::new(self.expression(target)),
                    name: name.text.as_str().into(),
                    access: *access,
                },
            },
            Syntax::MethodCall {
                target,
                name,
                access,
                arguments,
                ..
            } => {
                let arguments = self.arguments(arguments);
                match target {
                    None => self.function_call(&name.text, name.span, arguments),
                    Some(target) => match self.class_name(target) {
                        Some(class) => Kind::CallStatic {
                            class,
                            name: name.text.as_str().into(),
                            arguments,
                        },
                        None => Kind::CallMethod {
                            target: Box::new(self.expression(target)),
                            name: name.text.as_str().into(),
                            access: *access,
                            arguments,
                        },
                    },
                }
            }
            Syntax::Call { callee, arguments } => Kind::CallValue {
                callee: Box::new(self.expression(callee)),
                arguments: self.arguments(arguments),
            },
            Syntax::New { class, arguments } => Kind::New {
                class: self.type_name(class),
                arguments: self.arguments(arguments),
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
            Syntax::Binary {
                operator,
                left,
                right,
            } => self.binary(*operator, left, right),
            Syntax::Cast {
                operand,
                target_type,
            } => Kind::Cast {
                operand: Box::new(self.expression(operand)),
                class: self.type_name(target_type),
            },
            Syntax::InstanceOf {
                operand,
                target_type,
                negated,
            } => Kind::InstanceOf {
                operand: Box::new(self.expression(operand)),
                class: self.type_name(target_type),
                negated: *negated,
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

    /// A closure. Without a parameter list, or with `->` alone, it has the optional parameter `it`.
    fn closure(&mut self, closure: &ast::Closure, span: Span) -> Kind {
        self.scopes.push_frame(true);
        let explicit = closure.parameters.as_ref().filter(|list| !list.is_empty());
        let parameters = match explicit {
            Some(list) => list
                .iter()
                .map(|parameter| {
                    let slot = self.declare(&parameter.name);
                    let default = parameter
                        .default
                        .as_ref()
                        .map(|value| self.expression(value));
                    Parameter {
                        name: parameter.name.text.as_str().into(),
                        slot,
                        default,
                    }
                })
                .collect(),
            None => {
                self.scopes.declare("it", span);
                Vec::new()
            }
        };
        let body = self.block(&closure.body);
        let layout = self.scopes.pop_frame();
        Kind::Closure(Rc::new(Code {
            name: "closure".into(),
            parameters,
            implicit_parameter: explicit.is_none(),
            slot_count: layout.slot_count,
            captures: layout.captures,
            body,
            span,
        }))
    }

    fn arguments(&mut self, arguments: &[ast::Argument]) -> Arguments {
        let mut lowered = Arguments::default();
        for argument in arguments {
            let value = self.expression(&argument.value);
            match &argument.name {
                Some(name) => lowered.named.push((name.text.as_str().into(), value)),
                None => lowered.positional.push(value),
            }
        }
        lowered
    }

    /// A call without a receiver: a function of the script or a built-in function.
    fn function_call(&mut self, name: &str, span: Span, arguments: Arguments) -> Kind {
        let function = if self.functions.contains(name) {
            Function::Script(name.into())
        } else if SCRIPT_FUNCTIONS.contains(&name) {
            Function::Builtin(name.into())
        } else {
            self.error(format!("`{name}` is not defined"), span);
            return Kind::Literal(Value::Null);
        };
        Kind::CallFunction {
            function,
            arguments,
        }
    }

    /// The class a receiver names, unless a variable of that name hides it.
    fn class_name(&mut self, target: &ast::Expression) -> Option<Rc<str>> {
        match &target.kind {
            Syntax::Variable(name)
                if CLASSES.contains(&name.as_str()) && self.scopes.resolve(name).is_none() =>
            {
                Some(name.as_str().into())
            }
            _ => None,
        }
    }

    /// The simple name of a type, without package or type arguments.
    fn type_name(&mut self, class: &ast::Type) -> Rc<str> {
        let name = class
            .name
            .parts
            .last()
            .map(|part| part.text.as_str())
            .unwrap_or("");
        name.into()
    }

    fn binary(&mut self, operator: B, left: &ast::Expression, right: &ast::Expression) -> Kind {
        let left = Box::new(self.expression(left));
        let right = Box::new(self.expression(right));
        match operator {
            B::Power => binary(Operator::Power, left, right),
            B::Multiply => binary(Operator::Multiply, left, right),
            B::Divide => binary(Operator::Divide, left, right),
            B::Modulo => binary(Operator::Modulo, left, right),
            B::Add => binary(Operator::Add, left, right),
            B::Subtract => binary(Operator::Subtract, left, right),
            B::LeftShift => binary(Operator::LeftShift, left, right),
            B::RightShift => binary(Operator::RightShift, left, right),
            B::UnsignedShift => binary(Operator::UnsignedShift, left, right),
            B::BitAnd => binary(Operator::BitAnd, left, right),
            B::Xor => binary(Operator::Xor, left, right),
            B::BitOr => binary(Operator::BitOr, left, right),
            B::RangeInclusive | B::RangeExclusiveRight => Kind::Range {
                from: left,
                to: right,
                exclusive: operator == B::RangeExclusiveRight,
            },
            B::Less => compare(Relation::Less, left, right),
            B::LessEqual => compare(Relation::LessEqual, left, right),
            B::Greater => compare(Relation::Greater, left, right),
            B::GreaterEqual => compare(Relation::GreaterEqual, left, right),
            B::Equal | B::NotEqual => Kind::Equal {
                negated: operator == B::NotEqual,
                left,
                right,
            },
            B::Spaceship => Kind::Spaceship { left, right },
            B::In | B::NotIn => Kind::In {
                negated: operator == B::NotIn,
                item: left,
                container: right,
            },
            B::RegexFind | B::RegexMatch => Kind::Regex {
                matches: operator == B::RegexMatch,
                text: left,
                pattern: right,
            },
            B::And => Kind::And { left, right },
            B::Or => Kind::Or { left, right },
        }
    }
}

fn binary(operator: Operator, left: Box<Expression>, right: Box<Expression>) -> Kind {
    Kind::Binary {
        operator,
        left,
        right,
    }
}

fn compare(relation: Relation, left: Box<Expression>, right: Box<Expression>) -> Kind {
    Kind::Relation {
        relation,
        left,
        right,
    }
}

fn literal_string(text: &str, span: Span) -> Expression {
    Expression {
        kind: Kind::Literal(Value::string(text)),
        span,
    }
}
