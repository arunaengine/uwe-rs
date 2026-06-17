//! Static methods of Java classes that scripts and configs call, like `System.getenv('X')`
//! and `Math.max(a, b)`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_parser::Span;

use crate::number::Number;
use crate::units::java_round;
use crate::value::{JsonOptions, State, Value};

use super::super::{AtSpan, Error, Interpreter};
use super::{format, json};

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
            ("Pattern", "compile", [text]) => {
                let pattern = crate::regex::Pattern::new(&text.to_string()).at(span)?;
                Value::Pattern(std::rc::Rc::new(pattern))
            }
            ("Pattern", "quote", [text]) => Value::string(format!("\\Q{text}\\E")),
            ("Pattern", "matches", [regex, text]) => {
                let pattern = crate::regex::Pattern::new(&regex.to_string()).at(span)?;
                Value::Boolean(pattern.matches(&text.to_string()))
            }
            // Numbers are formatted as the root locale does, whatever locale is given.
            ("String", "format", [Value::Object(locale), pattern, rest @ ..])
                if matches!(locale.borrow().state, State::Locale(_)) =>
            {
                Value::string(format::format(&pattern.to_string(), rest).at(span)?)
            }
            ("String", "format", [pattern, rest @ ..]) => {
                Value::string(format::format(&pattern.to_string(), rest).at(span)?)
            }
            ("MessageDigest", "getInstance", [algorithm]) => {
                super::bytes::message_digest(&algorithm.to_string()).at(span)?
            }
            ("Arrays", "asList", [Value::List(list)]) => Value::list(list.borrow().items.clone()),
            ("Arrays", "asList", items) => Value::list(items.to_vec()),
            ("Files", "size", [Value::Path(path)]) => match self.host.metadata(path) {
                Ok(Some(metadata)) => Value::Number(Number::Long(metadata.size as i64)),
                Ok(None) => {
                    let exception =
                        crate::exception::Exception::new("NoSuchFileException", path.text());
                    return Err(super::super::thrown(exception, span));
                }
                Err(error) => return Err(error.or_at(span)),
            },
            ("Files", "newInputStream", [Value::Path(path)]) => {
                let bytes = self.host.read(path).map_err(|error| error.or_at(span))?;
                super::stream(bytes)
            }
            ("FilesEx", "copyTo", [source @ Value::Path(_), target]) => {
                let copied = self.call_method(source, "copyTo", vec![target.clone()], span)?;
                return Ok(Some(copied));
            }
            // The functions of every script are static methods of `nextflow.Nextflow`, which
            // Groovy classes call by that name.
            ("Nextflow", "file" | "files" | "error" | "tuple" | "groupKey", _) => {
                return self.call_builtin(name, arguments.to_vec(), span).map(Some);
            }
            ("SplitterEx", "splitCsv", [source, options @ ..]) => {
                return self
                    .call_method(source, "splitCsv", options.to_vec(), span)
                    .map(Some);
            }
            ("SchemaLoader", "load", [json]) => super::org_json::load_schema(self, json, span)?,
            // Java's parsers of numbers, like `Integer.parseInt`, as Groovy's `toInteger()`.
            (
                "Integer" | "Long" | "Double" | "Float",
                "parseInt" | "parseLong" | "parseDouble" | "parseFloat" | "valueOf",
                [Value::String(text) | Value::GString(text)],
            ) => {
                let method = format!("to{class}");
                super::string::parse_number(&method, text).at(span)?
            }
            ("JsonOutput", "toJson", [value]) => {
                Value::string(self.json_text(value, &JsonOptions::default(), span)?)
            }
            ("JsonOutput", "prettyPrint", [text]) => {
                Value::string(json::pretty_print(&text.to_string()).at(span)?)
            }
            // `System.exit(code)` ends the run with the code, as the legacy `exit code` does.
            ("System", "exit", [code @ Value::Number(_)]) => {
                return self
                    .call_builtin("exit", vec![code.clone()], span)
                    .map(Some);
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
            // The separators of paths on Linux, where tasks run.
            ("File", "separator") => Some(Value::string("/")),
            ("File", "pathSeparator") => Some(Value::string(":")),
            // The kinds of files that `eachFile` and `eachFileRecurse` visit.
            ("FileType", "FILES" | "DIRECTORIES" | "ANY") => Some(Value::string(name)),
            ("Locale", "ROOT" | "US" | "UK" | "ENGLISH") => {
                let tag = match name {
                    "ROOT" => "",
                    "US" => "en_US",
                    "UK" => "en_GB",
                    _ => "en",
                };
                Some(Value::Object(std::rc::Rc::new(std::cell::RefCell::new(
                    crate::value::Object {
                        class: "java.util.Locale",
                        state: State::Locale(tag.into()),
                    },
                ))))
            }
            // `System.out` and `System.err` print through the host.
            ("System", "out" | "err") => Some(Value::Namespace(format!("System.{name}").into())),
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
