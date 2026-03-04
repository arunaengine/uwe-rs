//! Methods of `java.util.regex.Pattern` and `Matcher` values.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use crate::number::Number;
use crate::regex::{Matcher, Pattern};
use crate::value::Value;

pub(super) fn pattern_method(
    pattern: &Rc<Pattern>,
    name: &str,
    arguments: &[Value],
) -> Option<Value> {
    match (name, arguments) {
        ("pattern" | "toString", []) => Some(Value::string(pattern.source.as_str())),
        ("matcher", [text]) => {
            let matcher = Matcher::new(pattern.clone(), text.to_groovy_string());
            Some(Value::Matcher(Rc::new(matcher)))
        }
        _ => None,
    }
}

pub(super) fn matcher_method(matcher: &Matcher, name: &str, arguments: &[Value]) -> Option<Value> {
    match (name, arguments) {
        // `getCount()` is an `Integer`, but `size()` is a `Long` (Groovy spec, section 8.6).
        ("getCount", []) => Some(Value::integer(matcher.matches.len() as i32)),
        ("size", []) => Some(Value::Number(Number::Long(matcher.matches.len() as i64))),
        ("matches", []) => Some(Value::Boolean(matcher.pattern.matches(&matcher.text))),
        ("groupCount", []) => Some(Value::integer(matcher.pattern.group_count() as i32)),
        _ => None,
    }
}
