//! Methods of `java.util.regex.Pattern` and `Matcher` values.
// Copyright (c) 2026 The uwe Contributors
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

/// The matches as Groovy iterates a matcher: the list of the groups of each match when the
/// pattern has groups, else the text of each match.
pub(super) fn matcher_items(matcher: &Matcher) -> Vec<Value> {
    let text = |group: &Option<String>| group.as_deref().map_or(Value::Null, Value::string);
    matcher
        .matches
        .iter()
        .map(|groups| match matcher.pattern.group_count() {
            0 => text(&groups[0]),
            _ => Value::list(groups.iter().map(text).collect()),
        })
        .collect()
}

pub(super) fn matcher_method(matcher: &Matcher, name: &str, arguments: &[Value]) -> Option<Value> {
    match (name, arguments) {
        // `getCount()` is an `Integer`, but `size()` is a `Long` (Groovy spec, section 8.6).
        ("getCount", []) => Some(Value::integer(matcher.matches.len() as i32)),
        ("size", []) => Some(Value::Number(Number::Long(matcher.matches.len() as i64))),
        ("matches", []) => Some(Value::Boolean(matcher.pattern.matches(&matcher.text))),
        ("groupCount", []) => Some(Value::integer(matcher.pattern.group_count() as i32)),
        // Java's stream of the matches, by the text of each.
        ("results", []) => {
            let texts = matcher
                .matches
                .iter()
                .map(|groups| groups[0].as_deref().map_or(Value::Null, Value::string))
                .collect();
            Some(Value::List(Rc::new(std::cell::RefCell::new(
                crate::value::List::new(crate::value::ListKind::Stream, texts),
            ))))
        }
        ("find", []) => {
            let next = matcher.current.get().map_or(0, |current| current + 1);
            let found = next < matcher.matches.len();
            matcher.current.set(Some(next.min(matcher.matches.len())));
            Some(Value::Boolean(found))
        }
        ("group", [] | [Value::Number(_)]) => {
            let group = match arguments {
                [Value::Number(group)] => group.to_i64() as usize,
                _ => 0,
            };
            let found = matcher
                .current
                .get()
                .and_then(|current| matcher.matches.get(current))?;
            Some(
                found
                    .get(group)
                    .cloned()
                    .flatten()
                    .map_or(Value::Null, Value::string),
            )
        }
        _ => None,
    }
}
