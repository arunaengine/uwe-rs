//! The standard library: methods of values by their Java class. It grows with what the
//! nf-core corpus uses; a method it lacks is an error that says so.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod string;

use nextflow_parser::Span;

use super::{Error, Interpreter};
use crate::value::Value;

impl Interpreter<'_> {
    /// A library method of the receiver's class, or `None` when the class has no method of
    /// that name for these arguments.
    pub(super) fn library_method(
        &mut self,
        object: &Value,
        name: &str,
        arguments: &[Value],
        span: Span,
    ) -> Result<Option<Value>, Error> {
        let found = match object {
            Value::String(text) | Value::GString(text) => {
                self.string_method(text, name, arguments, span)?
            }
            _ => None,
        };
        if found.is_some() {
            return Ok(found);
        }
        Ok(object_method(object, name, arguments))
    }
}

/// Methods every object has: Groovy's `toString`, `equals` and `hashCode`.
fn object_method(object: &Value, name: &str, arguments: &[Value]) -> Option<Value> {
    Some(match (name, arguments) {
        ("toString", []) => Value::string(object.to_groovy_string()),
        ("equals", [other]) => Value::Boolean(match object {
            Value::List(_) | Value::Map(_) | Value::Set(_) => object.groovy_equals(other),
            _ => object.java_equals(other),
        }),
        ("hashCode", []) => Value::integer(object.java_hash()),
        _ => return None,
    })
}
