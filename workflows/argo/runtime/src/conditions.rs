//! Evaluates resource success and failure conditions as Kubernetes label selectors.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt;

use crate::value::Value;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Operator {
    Equals,
    DoubleEquals,
    NotEquals,
    In,
    NotIn,
    Exists,
    DoesNotExist,
    GreaterThan,
    LessThan,
}

/// One requirement of a selector, like `status.phase in (Failed, Error)`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Requirement {
    key: String,
    operator: Operator,
    values: Vec<String>,
}

impl Requirement {
    /// Whether the resource meets the requirement, as `labels.Requirement.Matches` decides it.
    pub fn matches(&self, resource: &Value) -> bool {
        let found = lookup(resource, &self.key);
        let listed = found
            .as_ref()
            .is_some_and(|text| self.values.contains(text));
        match self.operator {
            Operator::Equals | Operator::DoubleEquals | Operator::In => listed,
            Operator::NotEquals | Operator::NotIn => !listed,
            Operator::Exists => found.is_some(),
            Operator::DoesNotExist => found.is_none(),
            Operator::GreaterThan | Operator::LessThan => {
                let have = found.and_then(|text| text.parse::<i64>().ok());
                let want = self
                    .values
                    .first()
                    .and_then(|text| text.parse::<i64>().ok());
                match (have, want) {
                    (Some(have), Some(want)) if self.operator == Operator::GreaterThan => {
                        have > want
                    }
                    (Some(have), Some(want)) => have < want,
                    _ => false,
                }
            }
        }
    }
}

/// The requirement as Kubernetes prints it, with the values of a set sorted.
impl fmt::Display for Requirement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.operator == Operator::DoesNotExist {
            f.write_str("!")?;
        }
        f.write_str(&self.key)?;
        let operator = match self.operator {
            Operator::Equals => "=",
            Operator::DoubleEquals => "==",
            Operator::NotEquals => "!=",
            Operator::In => " in ",
            Operator::NotIn => " notin ",
            Operator::GreaterThan => ">",
            Operator::LessThan => "<",
            Operator::Exists | Operator::DoesNotExist => return Ok(()),
        };
        let mut values = self.values.clone();
        values.sort();
        match self.operator {
            Operator::In | Operator::NotIn => write!(f, "{operator}({})", values.join(",")),
            _ => write!(f, "{operator}{}", values.join(",")),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Word(String),
    Symbol(&'static str),
}

const SYMBOLS: [&str; 9] = ["!=", "==", "!", "(", ")", ",", "=", ">", "<"];

fn tokens(selector: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut rest = selector;
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            return tokens;
        }
        if let Some(symbol) = SYMBOLS.iter().find(|symbol| rest.starts_with(**symbol)) {
            tokens.push(Token::Symbol(symbol));
            rest = &rest[symbol.len()..];
            continue;
        }
        let end = rest
            .find(|c: char| c.is_whitespace() || "!(),=<>".contains(c))
            .unwrap_or(rest.len());
        tokens.push(Token::Word(rest[..end].to_owned()));
        rest = &rest[end..];
    }
}

/// Parses a selector into its requirements, sorted by key as `labels.Parse` sorts them.
pub(crate) fn parse(selector: &str) -> Result<Vec<Requirement>, String> {
    let tokens = tokens(selector);
    let mut position = 0;
    let mut next = || {
        let token = tokens.get(position).cloned();
        position += 1;
        token
    };
    let mut requirements = Vec::new();
    let mut token = next();
    while let Some(first) = token {
        let (key, mut operator) = match first {
            Token::Symbol("!") => match next() {
                Some(Token::Word(key)) => (key, Some(Operator::DoesNotExist)),
                _ => return Err("expected a key after '!'".into()),
            },
            Token::Word(key) => (key, None),
            Token::Symbol(symbol) => return Err(format!("found '{symbol}', expected: key")),
        };
        let mut values = Vec::new();
        token = next();
        if operator.is_none() {
            operator = Some(match &token {
                None | Some(Token::Symbol(",")) => Operator::Exists,
                Some(Token::Symbol("=")) => Operator::Equals,
                Some(Token::Symbol("==")) => Operator::DoubleEquals,
                Some(Token::Symbol("!=")) => Operator::NotEquals,
                Some(Token::Symbol(">")) => Operator::GreaterThan,
                Some(Token::Symbol("<")) => Operator::LessThan,
                Some(Token::Word(word)) if word == "in" => Operator::In,
                Some(Token::Word(word)) if word == "notin" => Operator::NotIn,
                Some(Token::Word(found)) => {
                    return Err(format!(
                        "found '{found}', expected: in, notin, =, ==, !=, gt, lt"
                    ));
                }
                Some(Token::Symbol(found)) => {
                    return Err(format!(
                        "found '{found}', expected: in, notin, =, ==, !=, gt, lt"
                    ));
                }
            });
        }
        let operator = operator.unwrap_or(Operator::Exists);
        match operator {
            Operator::Exists | Operator::DoesNotExist => {}
            Operator::In | Operator::NotIn => {
                if next() != Some(Token::Symbol("(")) {
                    return Err("expected '(' after in or notin".into());
                }
                let mut value = String::new();
                loop {
                    match next() {
                        Some(Token::Word(word)) => value = word,
                        Some(Token::Symbol(",")) => values.push(std::mem::take(&mut value)),
                        Some(Token::Symbol(")")) => {
                            if !value.is_empty() || !values.is_empty() {
                                values.push(value);
                            }
                            break;
                        }
                        _ => return Err("expected ')' at the end of a set of values".into()),
                    }
                }
                token = next();
            }
            _ => {
                token = next();
                match &token {
                    Some(Token::Word(word)) => {
                        values.push(word.clone());
                        token = next();
                    }
                    None | Some(Token::Symbol(",")) => values.push(String::new()),
                    Some(Token::Symbol(found)) => {
                        return Err(format!("found '{found}', expected: identifier"));
                    }
                }
                let numeric = values.iter().all(|value| value.parse::<i64>().is_ok());
                if matches!(operator, Operator::GreaterThan | Operator::LessThan) && !numeric {
                    return Err(format!(
                        "for 'gt', 'lt' operators, exactly one integer value is required, not {}",
                        values.join(",")
                    ));
                }
            }
        }
        requirements.push(Requirement {
            key,
            operator,
            values,
        });
        match token {
            None => break,
            Some(Token::Symbol(",")) => token = next(),
            Some(Token::Word(found)) => return Err(format!("found '{found}', expected: ','")),
            Some(Token::Symbol(found)) => return Err(format!("found '{found}', expected: ','")),
        }
    }
    requirements.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(requirements)
}

/// The value at a gjson path like `status.phase`, as gjson's `String()` gives it: texts as
/// they are, other values as JSON. A missing value is `None`.
fn lookup(resource: &Value, path: &str) -> Option<String> {
    let mut value = resource;
    for part in path.split('.') {
        value = match value {
            Value::Map(entries) => entries.get(part)?,
            Value::Array(items) if part == "#" => return Some(items.len().to_string()),
            Value::Array(items) => items.get(part.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(match value {
        Value::String(text) => text.clone(),
        Value::Nil => String::new(),
        other => other.to_json(),
    })
}

/// How the conditions of a resource template judge the resource now.
#[derive(Debug, PartialEq)]
pub(crate) enum Verdict {
    /// A failure requirement holds; the message names it.
    Failed(String),
    Succeeded,
    /// Neither holds yet.
    Waiting,
}

/// Applies the conditions as Argo's `matchConditions`: each failure requirement on its own
/// fails, and the success requirements must all hold. Without a success condition, a resource
/// that does not fail succeeds.
pub(crate) fn judge(success: &str, failure: &str, resource: &Value) -> Result<Verdict, String> {
    let parsed = |kind: &str, condition: &str| {
        if condition.is_empty() {
            return Ok(Vec::new());
        }
        parse(condition)
            .map_err(|error| format!("{kind} condition '{condition}' failed to parse: {error}"))
    };
    let (success, failure) = (parsed("success", success)?, parsed("fail", failure)?);
    for requirement in &failure {
        if requirement.matches(resource) {
            return Ok(Verdict::Failed(format!(
                "failure condition '{requirement}' evaluated true"
            )));
        }
    }
    if success
        .iter()
        .all(|requirement| requirement.matches(resource))
    {
        return Ok(Verdict::Succeeded);
    }
    Ok(Verdict::Waiting)
}

#[cfg(test)]
#[path = "conditions_tests.rs"]
mod tests;
