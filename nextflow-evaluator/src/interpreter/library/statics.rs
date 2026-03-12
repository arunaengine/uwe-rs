//! Static methods of Java classes that scripts and configs call, like `System.getenv('X')`
//! and `Math.max(a, b)`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_parser::Span;

use crate::number::Number;
use crate::units::java_round;
use crate::value::{JsonOptions, Value};

use super::super::{AtSpan, Error, Interpreter};
use super::json;

impl Interpreter<'_> {
    /// `Class.name(arguments)`, or `None` when the class has no such method.
    pub(in crate::interpreter) fn static_method(
        &mut self,
        class: &str,
        name: &str,
        arguments: &[Value],
        span: Span,
    ) -> Result<Option<Value>, Error> {
        Ok(Some(match (class, name, arguments) {
            ("Arrays", "asList", [Value::List(list)]) => Value::list(list.borrow().items.clone()),
            ("Arrays", "asList", items) => Value::list(items.to_vec()),
            ("FilesEx", "copyTo", [source @ Value::Path(_), target]) => {
                let copied = self.call_method(source, "copyTo", vec![target.clone()], span)?;
                return Ok(Some(copied));
            }
            ("JsonOutput", "toJson", [value]) => {
                Value::string(self.json_text(value, &JsonOptions::default(), span)?)
            }
            ("JsonOutput", "prettyPrint", [text]) => {
                Value::string(json::pretty_print(&text.to_string()).at(span)?)
            }
            ("System", "getenv", [variable]) => self
                .host
                .environment(&variable.to_string())
                .map_or(Value::Null, Value::string),
            ("System", "getProperty", [property]) => property_value(&property.to_string()),
            ("System", "currentTimeMillis", []) => Value::Number(Number::Long(crate::date::now())),
            ("Math", _, [Value::Number(a), Value::Number(b)]) => match math2(name, a, b) {
                Some(value) => value,
                None => return Ok(None),
            },
            ("Math", _, [Value::Number(a)]) => match math1(name, a) {
                Some(value) => value,
                None => return Ok(None),
            },
            _ => return Ok(None),
        }))
    }
}

impl Interpreter<'_> {
    /// `Class.name`: a constant like `Integer.MAX_VALUE`, or a property of the class itself.
    pub(in crate::interpreter) fn static_property(
        &mut self,
        class: &str,
        name: &str,
    ) -> Option<Value> {
        let number = |number| Some(Value::Number(number));
        match (class, name) {
            ("Integer", "MAX_VALUE") => number(Number::Integer(i32::MAX)),
            ("Integer", "MIN_VALUE") => number(Number::Integer(i32::MIN)),
            ("Double", "POSITIVE_INFINITY") => number(Number::Double(f64::INFINITY)),
            ("Double", "NEGATIVE_INFINITY") => number(Number::Double(f64::NEG_INFINITY)),
            ("Double", "NaN") => number(Number::Double(f64::NAN)),
            ("Double", "MAX_VALUE") => number(Number::Double(f64::MAX)),
            ("Long", "MAX_VALUE") => number(Number::Long(i64::MAX)),
            ("Long", "MIN_VALUE") => number(Number::Long(i64::MIN)),
            (_, "name") => crate::value::qualified_class_name(class).map(Value::string),
            (_, "simpleName") => {
                crate::value::qualified_class_name(class).map(|_| Value::string(class))
            }
            _ => None,
        }
    }
}

/// `System.getProperty` for the properties that pipelines read.
fn property_value(name: &str) -> Value {
    let variable = |name: &str| std::env::var(name).ok();
    let value = match name {
        "user.name" => variable("USER"),
        "user.home" => variable("HOME"),
        "user.dir" => std::env::current_dir()
            .ok()
            .map(|dir| dir.to_string_lossy().into_owned()),
        "java.io.tmpdir" => Some("/tmp".into()),
        "line.separator" => Some("\n".into()),
        "file.separator" => Some("/".into()),
        _ => None,
    };
    value.map_or(Value::Null, Value::string)
}

/// Java's overloads by argument type: `int` if both are, else `long`, else `double`.
fn math2(name: &str, a: &Number, b: &Number) -> Option<Value> {
    // Groovy finds `Math.max(1, 2.5)` ambiguous, so big numbers are left out.
    let big = |n: &Number| matches!(n, Number::BigInteger(_) | Number::BigDecimal(_));
    if big(a) || big(b) {
        return None;
    }
    let integral = |n: &Number| matches!(n, Number::Integer(_) | Number::Long(_));
    let number = match name {
        "max" | "min" => {
            let pick_first = if name == "max" {
                a.to_f64() >= b.to_f64()
            } else {
                a.to_f64() <= b.to_f64()
            };
            match (a, b) {
                (Number::Integer(_), Number::Integer(_)) => {
                    let value = if pick_first { a } else { b };
                    Number::Integer(value.to_i32())
                }
                _ if integral(a) && integral(b) => {
                    Number::Long(if pick_first { a.to_i64() } else { b.to_i64() })
                }
                _ => Number::Double(if name == "max" {
                    a.to_f64().max(b.to_f64())
                } else {
                    a.to_f64().min(b.to_f64())
                }),
            }
        }
        "pow" => Number::Double(a.to_f64().powf(b.to_f64())),
        _ => return None,
    };
    Some(Value::Number(number))
}

fn math1(name: &str, a: &Number) -> Option<Value> {
    let value = a.to_f64();
    let number = match name {
        "ceil" => Number::Double(value.ceil()),
        "floor" => Number::Double(value.floor()),
        "sqrt" => Number::Double(value.sqrt()),
        "log" => Number::Double(value.ln()),
        "log10" => Number::Double(value.log10()),
        "exp" => Number::Double(value.exp()),
        "round" => match a {
            Number::Float(value) => Number::Integer(java_round(f64::from(*value)) as i32),
            _ => Number::Long(java_round(value)),
        },
        "abs" => match a {
            Number::Integer(value) => Number::Integer(value.wrapping_abs()),
            Number::Long(value) => Number::Long(value.wrapping_abs()),
            _ => Number::Double(value.abs()),
        },
        _ => return None,
    };
    Some(Value::Number(number))
}
