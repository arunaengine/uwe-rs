//! `string.Template` and `string.Formatter`: `$name` substitution, and the parts of a
//! `str.format` template.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::any::Any;

use crate::error::Error;
use crate::format::{Accessor, Piece, parse_template, plain_repr};
use crate::interpreter::Interpreter;
use crate::value::{Args, Native, Value};

/// `string.Template(template)`.
pub struct Template(String);

impl Template {
    pub fn value(text: String) -> Value {
        Value::native(Template(text))
    }

    /// The text with `$name`, `${name}` and `$$` replaced; a missing name is a `KeyError`
    /// unless `safe` keeps its placeholder.
    fn substitute(
        &self,
        interpreter: &mut Interpreter,
        mut args: Args,
        safe: bool,
    ) -> Result<Value, Error> {
        let mapping = args.take(0, "mapping");
        let lookup = |interpreter: &mut Interpreter, name: &str| -> Result<Option<Value>, Error> {
            if let Some((_, value)) = args.keywords.iter().find(|(key, _)| &**key == name) {
                return Ok(Some(value.clone()));
            }
            match &mapping {
                Some(mapping) => match interpreter.get_item(mapping, &Value::str(name)) {
                    Ok(value) => Ok(Some(value)),
                    Err(error) if error.is("KeyError") => Ok(None),
                    Err(error) => Err(error),
                },
                None => Ok(None),
            }
        };
        let chars: Vec<char> = self.0.chars().collect();
        let mut output = String::new();
        let mut index = 0;
        while index < chars.len() {
            if chars[index] != '$' {
                output.push(chars[index]);
                index += 1;
                continue;
            }
            let next = chars.get(index + 1).copied();
            if next == Some('$') {
                output.push('$');
                index += 2;
                continue;
            }
            let (name, end) = if next == Some('{') {
                match chars[index + 2..].iter().position(|c| *c == '}') {
                    Some(close) => (
                        chars[index + 2..index + 2 + close]
                            .iter()
                            .collect::<String>(),
                        index + 3 + close,
                    ),
                    None => (String::new(), index + 1),
                }
            } else {
                let length = chars[index + 1..]
                    .iter()
                    .take_while(|c| c.is_ascii_alphanumeric() || **c == '_')
                    .count();
                let name: String = chars[index + 1..index + 1 + length].iter().collect();
                (name, index + 1 + length)
            };
            let valid = name
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
            if !valid {
                if safe {
                    output.push('$');
                    index += 1;
                    continue;
                }
                return Err(Error::value_error(format!(
                    "Invalid placeholder in string: line 1, col {}",
                    index + 1
                )));
            }
            match lookup(interpreter, &name)? {
                Some(value) => output.push_str(&interpreter.to_str(&value)?),
                None if safe => output.extend(&chars[index..end]),
                None => return Err(Error::key_error(Value::str(name))),
            }
            index = end;
        }
        Ok(Value::str(output))
    }
}

impl Native for Template {
    fn type_name(&self) -> &str {
        "Template"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        this: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        let this = this.clone();
        Ok(Some(match name {
            "template" => Value::str(self.0.as_str()),
            "substitute" | "safe_substitute" => {
                let safe = name == "safe_substitute";
                Value::builtin(name, move |interpreter, args| {
                    let Some(template) = this.as_native::<Template>() else {
                        return Err(Error::type_error("not a Template"));
                    };
                    template.substitute(interpreter, args, safe)
                })
            }
            _ => return Ok(None),
        }))
    }
}

/// The full name of a replacement field, with its attributes and keys.
fn field_text(name: &str, accessors: &[Accessor]) -> String {
    let mut text = name.to_owned();
    for accessor in accessors {
        match accessor {
            Accessor::Attribute(attribute) => {
                text.push('.');
                text.push_str(attribute);
            }
            Accessor::Index(key) => {
                text.push('[');
                match key.as_str() {
                    Some(key) => text.push_str(key),
                    None => text.push_str(&plain_repr(key)),
                }
                text.push(']');
            }
        }
    }
    text
}

/// `string.Formatter()`: `parse`, `format` and `vformat`.
pub struct Formatter;

impl Native for Formatter {
    fn type_name(&self) -> &str {
        "Formatter"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        _: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        Ok(Some(match name {
            "parse" => Value::builtin("parse", |interpreter, args| {
                let template = interpreter.to_str(&args.get(0).cloned().unwrap_or(Value::None))?;
                let mut parts = Vec::new();
                let mut literal = String::new();
                for piece in parse_template(&template)? {
                    match piece {
                        Piece::Text(text) => literal.push_str(&text),
                        Piece::Field(field) => {
                            let conversion = field
                                .conversion
                                .map_or(Value::None, |c| Value::str(c.to_string()));
                            parts.push(Value::tuple(vec![
                                Value::str(std::mem::take(&mut literal)),
                                Value::str(field_text(&field.name, &field.accessors)),
                                Value::str(field.spec),
                                conversion,
                            ]));
                        }
                    }
                }
                if !literal.is_empty() {
                    parts.push(Value::tuple(vec![
                        Value::str(literal),
                        Value::None,
                        Value::None,
                        Value::None,
                    ]));
                }
                Ok(Value::iterator(parts))
            }),
            "format" => Value::builtin("format", |interpreter, mut args| {
                let template = args.positional.first().cloned().unwrap_or(Value::None);
                args.positional.remove(0);
                let method = interpreter.get_attribute(&template, "format")?;
                interpreter.call(&method, args)
            }),
            "vformat" => Value::builtin("vformat", |interpreter, args| {
                let template = args.get(0).cloned().unwrap_or(Value::None);
                let positional =
                    interpreter.iterate(args.get(1).unwrap_or(&Value::tuple(Vec::new())))?;
                let mut call = Args::new(positional);
                for (key, value) in interpreter
                    .mapping_items(args.get(2).unwrap_or(&Value::dict(Default::default())))?
                {
                    call.keywords
                        .push((interpreter.to_str(&key)?.into(), value));
                }
                let method = interpreter.get_attribute(&template, "format")?;
                interpreter.call(&method, call)
            }),
            _ => return Ok(None),
        }))
    }
}
