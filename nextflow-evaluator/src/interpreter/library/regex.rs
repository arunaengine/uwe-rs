//! Methods of `java.util.regex.Pattern` and `Matcher` values.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use crate::exception::Exception;
use crate::number::Number;
use crate::regex::{Matcher, Pattern};
use crate::value::Value;

pub(super) fn pattern_method(
    pattern: &Rc<Pattern>,
    name: &str,
    arguments: &[Value],
) -> Result<Option<Value>, Exception> {
    Ok(match (name, arguments) {
        ("pattern" | "toString", []) => Some(Value::string(pattern.source.as_str())),
        ("matcher", [text]) => {
            let matcher = Matcher::new(pattern.clone(), text.to_groovy_string())?;
            Some(Value::Matcher(Rc::new(matcher)))
        }
        _ => None,
    })
}

/// The matches as Groovy iterates a matcher: the list of the groups of each match when the
/// pattern has groups, else the text of each match.
pub(super) fn matcher_items(matcher: &Matcher) -> Result<Vec<Value>, Exception> {
    let text = |group: &Option<String>| group.as_deref().map_or(Value::Null, Value::string);
    Ok(matcher
        .pattern
        .all_groups(&matcher.text)?
        .iter()
        .map(|groups| match matcher.pattern.group_count() {
            0 => text(&groups[0]),
            _ => Value::list(groups.iter().map(text).collect()),
        })
        .collect())
}

pub(super) fn matcher_method(
    matcher: &Matcher,
    name: &str,
    arguments: &[Value],
) -> Result<Option<Value>, Exception> {
    Ok(match (name, arguments) {
        // `getCount()` is an `Integer`, but `size()` is a `Long` (Groovy spec, section 8.6).
        ("getCount", []) => Some(Value::integer(matcher.pattern.count(&matcher.text)? as i32)),
        ("size", []) => Some(Value::Number(Number::Long(
            matcher.pattern.count(&matcher.text)? as i64,
        ))),
        ("matches", []) => Some(Value::Boolean(matcher.pattern.matches(&matcher.text)?)),
        ("groupCount", []) => Some(Value::integer(matcher.pattern.group_count() as i32)),
        // Java's stream of the matches, by the text of each.
        ("results", []) => {
            let texts = matcher
                .pattern
                .all_groups(&matcher.text)?
                .iter()
                .map(|groups| groups[0].as_deref().map_or(Value::Null, Value::string))
                .collect();
            Some(Value::List(Rc::new(crate::value::ListCell::new(
                crate::value::List::new(crate::value::ListKind::Stream, texts),
            ))))
        }
        ("find", []) => Some(Value::Boolean(matcher.find()?)),
        ("group", [] | [Value::Number(_)]) => {
            let group = match arguments {
                [Value::Number(group)] => group.to_i64() as usize,
                _ => 0,
            };
            let current = matcher.current.borrow();
            let found = current
                .as_ref()
                .ok_or_else(|| Exception::new("IllegalStateException", "No match found"))?;
            Some(
                found
                    .get(group)
                    .cloned()
                    .flatten()
                    .map_or(Value::Null, Value::string),
            )
        }
        _ => None,
    })
}
