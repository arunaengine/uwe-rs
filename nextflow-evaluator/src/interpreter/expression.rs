//! Evaluating expressions.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use super::{AtSpan, Error, Frame, Interpreter, unsupported};
use crate::lowered::{Access, Code, Expression, ExpressionKind as Kind, Function, Part, Unary};
use crate::regex::{Matcher, Pattern};
use crate::value::{Closure, DefinitionRef, ListKind, Operator, PathValue, Value};

impl Interpreter<'_> {
    pub(super) fn expression(
        &mut self,
        frame: &mut Frame,
        expression: &Expression,
    ) -> Result<Value, Error> {
        let span = expression.span;
        Ok(match &expression.kind {
            Kind::Literal(value) => value.clone(),
            Kind::Variable(place) => frame.get(*place),
            Kind::Operator(name) => Value::Operator(Rc::new((name.clone(), Vec::new()))),
            Kind::Definition(name) => Value::Definition(Rc::new(DefinitionRef {
                module: frame.module.clone(),
                name: name.clone(),
            })),
            Kind::Builtin(name) if &**name == "moduleDir" => {
                let directory = frame.module.directory.borrow().clone();
                match directory {
                    Some(directory) => Value::Path(Rc::new(PathValue::parse(&directory, "/"))),
                    None => {
                        return Err(unsupported("`moduleDir` of a script without a file", span));
                    }
                }
            }
            Kind::Dynamic(name) => self.dynamic(name, span)?,
            Kind::Tap { source, target } => {
                let value = self.expression(frame, source)?;
                frame.set(*target, value.clone());
                value
            }
            Kind::Builtin(name) => match self.globals.get(name) {
                Some(value) => value.clone(),
                None => return Err(unsupported(format!("the built-in `{name}`"), span)),
            },
            Kind::Interpolate {
                parts,
                keep_gstring,
                escape_paths,
            } => {
                let mut text = String::new();
                for part in parts {
                    match part {
                        Part::Text(part) => text.push_str(part),
                        Part::Value(part) => match self.expression(frame, part)? {
                            Value::Closure(_) => {
                                return Err(unsupported("a closure inside a GString", part.span));
                            }
                            value if *escape_paths => text.push_str(&escaped(&value)),
                            value => text.push_str(&value.to_interpolated()),
                        },
                    }
                }
                if *keep_gstring {
                    Value::GString(text.into())
                } else {
                    Value::string(text)
                }
            }
            Kind::List(items) => Value::list(self.expressions(frame, items)?),
            Kind::Map(entries) => {
                let mut values = Vec::with_capacity(entries.len());
                for (key, value) in entries {
                    let key = self.expression(frame, key)?;
                    values.push((key, self.expression(frame, value)?));
                }
                Value::map(values)
            }
            Kind::Range {
                from,
                to,
                exclusive,
            } => {
                let from = self.expression(frame, from)?;
                let to = self.expression(frame, to)?;
                Value::range(from, to, *exclusive).at(span)?
            }
            Kind::Closure(code) => make_closure(frame, code),
            Kind::Property {
                target,
                name,
                access,
            } => {
                let object = self.expression(frame, target)?;
                self.spread(object, *access, |this, object| {
                    this.property(object, name, span)
                })?
            }
            Kind::CallMethod {
                target,
                name,
                access,
                arguments,
            } => {
                let object = self.expression(frame, target)?;
                let arguments = self.arguments(frame, arguments)?;
                self.spread(object, *access, |this, object| {
                    this.call_method(object, name, arguments.clone(), span)
                })?
            }
            Kind::CallFunction {
                function,
                arguments,
            } => {
                let arguments = self.arguments(frame, arguments)?;
                match function {
                    Function::Script(name) => {
                        let module = frame.module.clone();
                        self.call_script_function(&module, name, arguments, span)?
                    }
                    Function::Builtin(name) if &**name == "template" => {
                        let module = frame.module.clone();
                        self.template_file(&module, &arguments, span)?
                    }
                    Function::Builtin(name) => self.call_builtin(name, arguments, span)?,
                    Function::Definition(name) => {
                        let module = frame.module.clone();
                        self.call_definition(&module, name, arguments, span)?
                    }
                    Function::Operator(name) => Value::Operator(Rc::new((name.clone(), arguments))),
                }
            }
            Kind::CallValue { callee, arguments } => {
                let callee = self.expression(frame, callee)?;
                let arguments = self.arguments(frame, arguments)?;
                self.call_method(&callee, "call", arguments, span)?
            }
            Kind::CallStatic {
                class,
                name,
                arguments,
            } if &**class == "Channel" => {
                let arguments = self.arguments(frame, arguments)?;
                self.namespace_call("channel", name, arguments, span)?
            }
            Kind::CallStatic {
                class,
                name,
                arguments,
            } => {
                let arguments = self.arguments(frame, arguments)?;
                match self.static_method(class, name, &arguments, span)? {
                    Some(value) => value,
                    None => return Err(unsupported(format!("`{class}.{name}`"), span)),
                }
            }
            Kind::StaticProperty { class, name } if self.static_property(class, name).is_some() => {
                self.static_property(class, name).unwrap_or(Value::Null)
            }
            Kind::StaticProperty { class, name } => {
                return Err(unsupported(format!("`{class}.{name}`"), span));
            }
            Kind::Index { target, index } => {
                let object = self.expression(frame, target)?;
                let index = self.expressions(frame, index)?;
                self.index(&object, &index, span)?
            }
            Kind::Unary { operator, operand } => {
                let value = self.expression(frame, operand)?;
                match (operator, &value) {
                    (Unary::Not, _) => Value::Boolean(!value.truth()),
                    (Unary::Negate, _) => value.negate().at(span)?,
                    (Unary::Positive, Value::Number(_)) => value,
                    (Unary::BitwiseNot, Value::String(text) | Value::GString(text)) => {
                        Value::Pattern(Rc::new(Pattern::new(text).at(span)?))
                    }
                    (Unary::BitwiseNot, Value::Number(number)) => {
                        let result = number.bitwise_not().map_err(Into::into).at(span)?;
                        Value::Number(result)
                    }
                    _ => {
                        return Err(unsupported(
                            format!("{operator:?} on {}", value.class_name()),
                            span,
                        ));
                    }
                }
            }
            Kind::Binary {
                operator,
                left,
                right,
            } => {
                let left = self.expression(frame, left)?;
                let right = self.expression(frame, right)?;
                match (operator, &left, &right) {
                    (
                        Operator::BitOr,
                        Value::Channel(_) | Value::ChannelOut(_),
                        Value::Definition(_) | Value::Operator(_),
                    ) => self.pipe(left, &right, span)?,
                    _ => left.apply(*operator, &right).at(span)?,
                }
            }
            Kind::Relation {
                relation,
                left,
                right,
            } => {
                let left = self.expression(frame, left)?;
                let right = self.expression(frame, right)?;
                Value::Boolean(left.compare_relation(*relation, &right).at(span)?)
            }
            Kind::Equal {
                negated,
                left,
                right,
            } => {
                let left = self.expression(frame, left)?;
                let right = self.expression(frame, right)?;
                Value::Boolean(left.compare_equal(&right).at(span)? != *negated)
            }
            Kind::Spaceship { left, right } => {
                let left = self.expression(frame, left)?;
                let right = self.expression(frame, right)?;
                left.spaceship(&right).at(span)?
            }
            Kind::In {
                negated,
                item,
                container,
            } => {
                let item = self.expression(frame, item)?;
                let container = self.expression(frame, container)?;
                Value::Boolean(container.is_case(&item).at(span)? != *negated)
            }
            Kind::Regex {
                matches,
                text,
                pattern,
            } => {
                let text = self.expression(frame, text)?;
                let pattern = self.expression(frame, pattern)?;
                if *matches && (matches!(text, Value::Null) || matches!(pattern, Value::Null)) {
                    return Ok(Value::Boolean(false));
                }
                // A value other than a string is matched by its Groovy text.
                let text = text.to_groovy_string();
                let pattern = match pattern {
                    Value::Pattern(pattern) => pattern,
                    other => Rc::new(Pattern::new(&other.to_groovy_string()).at(span)?),
                };
                if *matches {
                    Value::Boolean(pattern.matches(&text))
                } else {
                    Value::Matcher(Rc::new(Matcher::new(pattern, text)))
                }
            }
            Kind::And { left, right } => Value::Boolean(
                self.expression(frame, left)?.truth() && self.expression(frame, right)?.truth(),
            ),
            Kind::Or { left, right } => Value::Boolean(
                self.expression(frame, left)?.truth() || self.expression(frame, right)?.truth(),
            ),
            Kind::Ternary {
                condition,
                then,
                otherwise,
            } => {
                if self.expression(frame, condition)?.truth() {
                    self.expression(frame, then)?
                } else {
                    self.expression(frame, otherwise)?
                }
            }
            Kind::Elvis { value, fallback } => {
                let value = self.expression(frame, value)?;
                if value.truth() {
                    value
                } else {
                    self.expression(frame, fallback)?
                }
            }
            Kind::Cast { operand, class } => {
                let value = self.expression(frame, operand)?;
                self.cast(value, class, span)?
            }
            Kind::InstanceOf {
                operand,
                class,
                negated,
            } => {
                let value = self.expression(frame, operand)?;
                Value::Boolean(super::access::instance_of(&value, class) != *negated)
            }
            Kind::New { class, arguments } => {
                let arguments = self.arguments(frame, arguments)?;
                self.new_object(class, arguments, span)?
            }
        })
    }

    fn expressions(
        &mut self,
        frame: &mut Frame,
        expressions: &[Expression],
    ) -> Result<Vec<Value>, Error> {
        expressions
            .iter()
            .map(|item| self.expression(frame, item))
            .collect()
    }

    /// Applies a property read or method call with `.`, `?.` or `*.` access. Spread access
    /// collects the results for the elements into a list; it is null-safe.
    fn spread(
        &mut self,
        object: Value,
        access: Access,
        mut apply: impl FnMut(&mut Self, &Value) -> Result<Value, Error>,
    ) -> Result<Value, Error> {
        match (access, &object) {
            (Access::Safe | Access::Spread, Value::Null) => Ok(Value::Null),
            (Access::Normal | Access::Safe, _) => apply(self, &object),
            (Access::Spread, Value::List(list)) => {
                let items = list.borrow().items.clone();
                let mut results = Vec::with_capacity(items.len());
                for item in &items {
                    results.push(match item {
                        Value::Null => Value::Null,
                        item => apply(self, item)?,
                    });
                }
                Ok(Value::list(results))
            }
            (Access::Spread, other) => apply(self, other),
        }
    }
}

/// A value inserted into a task script: a staged input file, or the files of one input, with
/// their special characters escaped (Nextflow's `LangHelpers.applyPathEscapeAware`).
fn escaped(value: &Value) -> String {
    match value {
        Value::Path(path) if path.staged.is_some() => escape_path(path.name()),
        Value::List(list) if list.borrow().kind == ListKind::BlankSeparated => {
            let names: Vec<String> = list
                .borrow()
                .items
                .iter()
                .map(|item| match item {
                    Value::Path(path) => escape_path(path.name()),
                    other => other.to_java_string(),
                })
                .collect();
            names.join(" ")
        }
        other => other.to_interpolated(),
    }
}

/// Nextflow's `Escape.path`: a backslash before each of `'"()\!&|<>`:;` and space. A `!`
/// right after an unescaped `[` with a `]` later is a glob complement and stays.
fn escape_path(name: &str) -> String {
    let characters: Vec<char> = name.chars().collect();
    let mut escaped = String::new();
    for (index, character) in characters.iter().enumerate() {
        let complement = *character == '!'
            && index > 0
            && characters[index - 1] == '['
            && (index < 2 || characters[index - 2] != '\\')
            && characters[index + 1..].contains(&']');
        if !complement && "'\" ()\\!&|<>`:;".contains(*character) {
            escaped.push('\\');
        }
        escaped.push(*character);
    }
    escaped
}

/// A closure value that shares the cells of the variables it captures.
fn make_closure(frame: &Frame, code: &Rc<Code>) -> Value {
    let captured = code
        .captures
        .iter()
        .map(|place| frame.cell(*place).clone())
        .collect();
    Value::Closure(Rc::new(Closure {
        code: code.clone(),
        module: frame.module.clone(),
        captured,
    }))
}
