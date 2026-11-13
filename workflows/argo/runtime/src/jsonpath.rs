//! Evaluates Argo JSONPath expressions over JSON values.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::expr::equal;
use crate::value::Value;

type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Debug, PartialEq)]
enum Selector {
    Key(String),
    Index(i64),
    Union(Vec<Selector>),
    Wildcard,
    Slice(Option<i64>, Option<i64>, Option<i64>),
    Filter(Filter),
    /// `..` before a selector: the selector applies at every depth.
    Descend(Box<Selector>),
}

/// Reads the value or values that `path` selects in `document`.
pub fn get(path: &str, document: &Value) -> Result<Value> {
    let selectors = parse(path)?;
    let definite = selectors
        .iter()
        .all(|selector| matches!(selector, Selector::Key(_) | Selector::Index(_)));
    let mut current = vec![document.clone()];
    for selector in &selectors {
        let mut next = Vec::new();
        for value in &current {
            select(selector, value, definite, &mut next)?;
        }
        current = next;
    }
    if definite {
        Ok(current.into_iter().next().unwrap_or(Value::Nil))
    } else {
        Ok(Value::Array(current))
    }
}

fn select(selector: &Selector, value: &Value, strict: bool, out: &mut Vec<Value>) -> Result<()> {
    match selector {
        Selector::Key(key) => match value {
            Value::Map(map) => match map.get(key) {
                Some(found) => out.push(found.clone()),
                None if strict => return Err(format!("key '{key}' not found")),
                None => {}
            },
            Value::Array(items) if key.parse::<i64>().is_ok() => {
                let index = key.parse::<i64>().unwrap_or_default();
                select(
                    &Selector::Index(index),
                    &Value::Array(items.clone()),
                    strict,
                    out,
                )?;
            }
            _ if strict => {
                return Err(format!("cannot get key '{key}' from {}", value.type_name()));
            }
            _ => {}
        },
        Selector::Index(index) => match value {
            Value::Array(items) => {
                let position = if *index < 0 {
                    items.len() as i64 + index
                } else {
                    *index
                };
                match usize::try_from(position)
                    .ok()
                    .and_then(|position| items.get(position))
                {
                    Some(found) => out.push(found.clone()),
                    None if strict => return Err(format!("index [{index}] out of range")),
                    None => {}
                }
            }
            Value::Map(map) => match map.get(&index.to_string()) {
                Some(found) => out.push(found.clone()),
                None if strict => return Err(format!("key '{index}' not found")),
                None => {}
            },
            _ if strict => {
                return Err(format!(
                    "cannot get index [{index}] from {}",
                    value.type_name()
                ));
            }
            _ => {}
        },
        Selector::Union(selectors) => {
            for selector in selectors {
                select(selector, value, false, out)?;
            }
        }
        Selector::Wildcard => match value {
            Value::Array(items) => out.extend(items.iter().cloned()),
            Value::Map(map) => out.extend(map.values().cloned()),
            _ => {}
        },
        Selector::Slice(start, end, step) => {
            if let Value::Array(items) = value {
                let length = items.len() as i64;
                let step = step.unwrap_or(1);
                if step == 0 {
                    return Err("slice step cannot be 0".into());
                }
                let clamp = |bound: i64| {
                    let bound = if bound < 0 { length + bound } else { bound };
                    bound.clamp(0, length)
                };
                if step > 0 {
                    let mut index = clamp(start.unwrap_or(0));
                    let end = clamp(end.unwrap_or(length));
                    while index < end {
                        out.push(items[index as usize].clone());
                        let Some(next) = index.checked_add(step) else {
                            break;
                        };
                        index = next;
                    }
                } else {
                    let mut index = start.map_or(length - 1, |start| clamp(start).min(length - 1));
                    let end = end.map_or(-1, clamp);
                    while index > end && index >= 0 {
                        out.push(items[index as usize].clone());
                        let Some(next) = index.checked_add(step) else {
                            break;
                        };
                        index = next;
                    }
                }
            }
        }
        Selector::Filter(filter) => {
            let candidates: Vec<&Value> = match value {
                Value::Array(items) => items.iter().collect(),
                Value::Map(map) => map.values().collect(),
                _ => Vec::new(),
            };
            for candidate in candidates {
                if filter.holds(candidate)? {
                    out.push(candidate.clone());
                }
            }
        }
        Selector::Descend(inner) => {
            fn walk(value: &Value, inner: &Selector, out: &mut Vec<Value>) -> Result<()> {
                select(inner, value, false, out)?;
                match value {
                    Value::Array(items) => items.iter().try_for_each(|item| walk(item, inner, out)),
                    Value::Map(map) => map.values().try_for_each(|item| walk(item, inner, out)),
                    _ => Ok(()),
                }
            }
            walk(value, inner, out)?;
        }
    }
    Ok(())
}

fn parse(path: &str) -> Result<Vec<Selector>> {
    let path = path.trim();
    let rest = path
        .strip_prefix('$')
        .ok_or_else(|| format!("invalid JSONPath `{path}`: it must start with $"))?;
    let characters: Vec<char> = rest.chars().collect();
    let mut selectors = Vec::new();
    let mut index = 0;
    while index < characters.len() {
        let descend = characters[index..].starts_with(&['.', '.']);
        let selector = if descend || characters[index] == '.' {
            index += if descend { 2 } else { 1 };
            if characters.get(index) == Some(&'[') {
                let (selector, next) = bracket(&characters, index)?;
                index = next;
                selector
            } else {
                let start = index;
                while index < characters.len() && !matches!(characters[index], '.' | '[') {
                    index += 1;
                }
                let name: String = characters[start..index].iter().collect();
                match name.as_str() {
                    "*" => Selector::Wildcard,
                    "" => return Err(format!("invalid JSONPath `{path}`: empty key")),
                    _ => Selector::Key(name),
                }
            }
        } else if characters[index] == '[' {
            let (selector, next) = bracket(&characters, index)?;
            index = next;
            selector
        } else {
            return Err(format!(
                "invalid JSONPath `{path}`: unexpected `{}`",
                characters[index]
            ));
        };
        selectors.push(if descend {
            Selector::Descend(Box::new(selector))
        } else {
            selector
        });
    }
    Ok(selectors)
}

/// A bracket selector from `[` to its `]`, and the position after it.
fn bracket(characters: &[char], start: usize) -> Result<(Selector, usize)> {
    let mut depth = 0;
    let mut quote = None;
    let mut end = start;
    while end < characters.len() {
        let character = characters[end];
        match quote {
            Some(open) if character == open => quote = None,
            Some(_) => {}
            None => match character {
                '\'' | '"' => quote = Some(character),
                '[' | '(' => depth += 1,
                ']' | ')' => {
                    depth -= 1;
                    if depth == 0 && character == ']' {
                        break;
                    }
                }
                _ => {}
            },
        }
        end += 1;
    }
    if end >= characters.len() {
        return Err("invalid JSONPath: unclosed `[`".into());
    }
    let inner: String = characters[start + 1..end].iter().collect();
    let inner = inner.trim();
    let selector = if inner == "*" {
        Selector::Wildcard
    } else if let Some(filter) = inner.strip_prefix('?') {
        let filter = filter.trim();
        let filter = filter
            .strip_prefix('(')
            .and_then(|filter| filter.strip_suffix(')'))
            .unwrap_or(filter);
        Selector::Filter(Filter::parse(filter)?)
    } else {
        let parts = split_top(inner, ',');
        let mut selectors = parts
            .iter()
            .map(|part| single(part.trim()))
            .collect::<Result<Vec<_>>>()?;
        if selectors.len() == 1 {
            selectors.remove(0)
        } else {
            Selector::Union(selectors)
        }
    };
    Ok((selector, end + 1))
}

fn single(part: &str) -> Result<Selector> {
    if let Some(key) = unquote(part) {
        return Ok(Selector::Key(key));
    }
    if part.contains(':') {
        let bounds: Vec<&str> = part.split(':').collect();
        let bound = |index: usize| -> Result<Option<i64>> {
            match bounds.get(index).map(|text| text.trim()) {
                None | Some("") => Ok(None),
                Some(text) => text
                    .parse()
                    .map(Some)
                    .map_err(|_| format!("invalid slice bound `{text}`")),
            }
        };
        return Ok(Selector::Slice(bound(0)?, bound(1)?, bound(2)?));
    }
    part.parse::<i64>()
        .map(Selector::Index)
        .or_else(|_| Ok(Selector::Key(part.to_owned())))
}

fn unquote(text: &str) -> Option<String> {
    let quote = text.chars().next()?;
    if (quote == '\'' || quote == '"') && text.len() >= 2 && text.ends_with(quote) {
        Some(text[1..text.len() - 1].to_owned())
    } else {
        None
    }
}

/// Splits at `separator` outside quotes and brackets.
fn split_top(text: &str, separator: char) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut depth = 0;
    let mut quote = None;
    for character in text.chars() {
        match quote {
            Some(open) if character == open => quote = None,
            Some(_) => {}
            None => match character {
                '\'' | '"' => quote = Some(character),
                '[' | '(' => depth += 1,
                ']' | ')' => depth -= 1,
                _ if character == separator && depth == 0 => {
                    parts.push(std::mem::take(&mut current));
                    continue;
                }
                _ => {}
            },
        }
        current.push(character);
    }
    parts.push(current);
    parts
}

/// A filter like `@.age > 42 && @.name == 'x'`.
#[derive(Clone, Debug, PartialEq)]
enum Filter {
    Or(Box<Filter>, Box<Filter>),
    And(Box<Filter>, Box<Filter>),
    Not(Box<Filter>),
    Compare(Operand, String, Operand),
    Exists(Operand),
}

#[derive(Clone, Debug, PartialEq)]
enum Operand {
    Current(Vec<Selector>),
    Literal(Value),
}

impl Filter {
    fn parse(text: &str) -> Result<Filter> {
        let text = text.trim();
        let or = split_operator(text, "||");
        if or.len() > 1 {
            return fold(&or, Filter::Or);
        }
        let and = split_operator(text, "&&");
        if and.len() > 1 {
            return fold(&and, Filter::And);
        }
        if let Some(inner) = text.strip_prefix('!') {
            return Ok(Filter::Not(Box::new(Filter::parse(inner)?)));
        }
        if text.starts_with('(')
            && text.ends_with(')')
            && split_top(&text[1..text.len() - 1], ',').len() == 1
        {
            return Filter::parse(&text[1..text.len() - 1]);
        }
        for operator in ["==", "!=", "<=", ">=", "=~", "<", ">"] {
            let parts = split_operator(text, operator);
            if parts.len() == 2 {
                return Ok(Filter::Compare(
                    Operand::parse(&parts[0])?,
                    operator.to_owned(),
                    Operand::parse(&parts[1])?,
                ));
            }
        }
        Ok(Filter::Exists(Operand::parse(text)?))
    }

    fn holds(&self, current: &Value) -> Result<bool> {
        Ok(match self {
            Filter::Or(left, right) => left.holds(current)? || right.holds(current)?,
            Filter::And(left, right) => left.holds(current)? && right.holds(current)?,
            Filter::Not(inner) => !inner.holds(current)?,
            Filter::Exists(operand) => operand.value(current).is_some(),
            Filter::Compare(left, operator, right) => {
                let (Some(left), Some(right)) = (left.value(current), right.value(current)) else {
                    return Ok(operator == "!=");
                };
                let order = match (&left, &right) {
                    (Value::String(a), Value::String(b)) => Some(a.cmp(b)),
                    (Value::Float(a), Value::Float(b)) => a.partial_cmp(b),
                    _ => None,
                };
                match operator.as_str() {
                    "==" => equal(&left, &right),
                    "!=" => !equal(&left, &right),
                    "=~" => match (&left, &right) {
                        (Value::String(text), Value::String(pattern)) => {
                            let pattern = pattern.trim_matches('/');
                            regex::Regex::new(pattern)
                                .map_err(|error| error.to_string())?
                                .is_match(text)
                        }
                        _ => false,
                    },
                    "<" => order == Some(std::cmp::Ordering::Less),
                    ">" => order == Some(std::cmp::Ordering::Greater),
                    "<=" => matches!(
                        order,
                        Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
                    ),
                    ">=" => matches!(
                        order,
                        Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)
                    ),
                    _ => false,
                }
            }
        })
    }
}

fn fold(parts: &[String], join: fn(Box<Filter>, Box<Filter>) -> Filter) -> Result<Filter> {
    let mut filters = parts.iter().map(|part| Filter::parse(part));
    let mut result = filters
        .next()
        .unwrap_or_else(|| Err("empty filter".into()))?;
    for filter in filters {
        result = join(Box::new(result), Box::new(filter?));
    }
    Ok(result)
}

/// Splits at `operator` outside quotes and brackets.
fn split_operator(text: &str, operator: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0;
    let mut quote = None;
    let mut start = 0;
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let character = bytes[index] as char;
        match quote {
            Some(open) if character == open => quote = None,
            Some(_) => {}
            None => match character {
                '\'' | '"' => quote = Some(character),
                '[' | '(' => depth += 1,
                ']' | ')' => depth -= 1,
                _ if depth == 0 && text[index..].starts_with(operator) => {
                    // `<` and `>` must not match the start of `<=` or `>=`, nor `=` of `==`.
                    let longer = ["<=", ">=", "==", "!=", "=~"]
                        .iter()
                        .any(|long| long.len() > operator.len() && text[index..].starts_with(long));
                    let inside = index > 0
                        && operator.len() == 1
                        && matches!(bytes[index - 1] as char, '<' | '>' | '=' | '!');
                    if !longer && !inside {
                        parts.push(text[start..index].to_owned());
                        index += operator.len();
                        start = index;
                        continue;
                    }
                }
                _ => {}
            },
        }
        index += 1;
    }
    parts.push(text[start..].to_owned());
    parts
}

impl Operand {
    fn parse(text: &str) -> Result<Operand> {
        let text = text.trim();
        if let Some(rest) = text.strip_prefix('@') {
            return Ok(Operand::Current(parse(&format!("${rest}"))?));
        }
        if text.starts_with('$') {
            return Err(format!(
                "filters that read the document root, like `{text}`, are not supported"
            ));
        }
        if let Some(literal) = unquote(text) {
            return Ok(Operand::Literal(Value::String(literal)));
        }
        if text.starts_with('/') {
            return Ok(Operand::Literal(Value::String(text.to_owned())));
        }
        match text {
            "true" => Ok(Operand::Literal(Value::Bool(true))),
            "false" => Ok(Operand::Literal(Value::Bool(false))),
            "null" => Ok(Operand::Literal(Value::Nil)),
            _ => text
                .parse::<f64>()
                .map(|number| Operand::Literal(Value::Float(number)))
                .map_err(|_| format!("invalid filter operand `{text}`")),
        }
    }

    fn value(&self, current: &Value) -> Option<Value> {
        let selectors = match self {
            Operand::Literal(value) => return Some(value.clone()),
            Operand::Current(selectors) => selectors,
        };
        let mut values = vec![current.clone()];
        for selector in selectors {
            let mut next = Vec::new();
            for value in &values {
                select(selector, value, false, &mut next).ok()?;
            }
            values = next;
        }
        values.into_iter().next()
    }
}

#[cfg(test)]
#[path = "jsonpath_tests.rs"]
mod tests;
