//! YTE templates in config files, as Snakemake 9.27 reads a config file with
//! `__use_yte__: true` through yte 1.9: values that start with `?` are Python expressions,
//! keys `?if`, `?elif`, `?else` and `?for` choose and repeat parts, `__definitions__` runs
//! Python statements and `__variables__` names values. The `doc` variable of yte, which reads
//! the rendered document so far, is left out.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use python_evaluator::{Dict, Error, Frame, Interpreter, Key, Scope, Source, Value};

use crate::workflow::workflow_error;

/// The key that turns templating on, at the top of the document.
const USE_YTE: &str = "__use_yte__";

/// A loaded config document with its templates rendered, when it asks for it with
/// `__use_yte__: true`; any other document as it is.
pub fn render(interpreter: &mut Interpreter, document: Value) -> Result<Value, Error> {
    let Value::Dict(dict) = document.plain() else {
        return Ok(document);
    };
    let key = Key::new(Value::str(USE_YTE))?;
    let enabled = dict.borrow().entries.get(&key).cloned();
    let Some(enabled) = enabled else {
        return Ok(document);
    };
    dict.borrow_mut().entries.shift_remove(&key);
    if !interpreter.truthy(&enabled)? {
        return Ok(document);
    }
    let scope = Scope::new(None);
    value(interpreter, &document, &scope)
}

fn value(interpreter: &mut Interpreter, item: &Value, scope: &Rc<Scope>) -> Result<Value, Error> {
    match item.plain() {
        Value::Dict(_) => mapping(interpreter, item, scope),
        Value::List(items) => {
            let items = items.borrow().clone();
            list(interpreter, &items, scope)
        }
        Value::Str(text) if text.starts_with('?') => evaluate(interpreter, &text[1..], scope),
        _ => Ok(item.clone()),
    }
}

/// A list, where an item `<: [...]` adds the items of its list.
fn list(interpreter: &mut Interpreter, items: &[Value], scope: &Rc<Scope>) -> Result<Value, Error> {
    let mut result = Vec::new();
    for item in items {
        if let Some(merged) = merge_key(interpreter, item)? {
            match value(interpreter, &merged, scope)?.plain() {
                Value::List(values) => result.extend(values.borrow().iter().cloned()),
                _ => {
                    return Err(workflow_error(
                        "Merge operator '<:' within a list must be followed by a list",
                    ));
                }
            }
        } else {
            result.push(value(interpreter, item, scope)?);
        }
    }
    Ok(Value::list(result))
}

/// The value of a mapping whose only key is `<`.
fn merge_key(interpreter: &mut Interpreter, item: &Value) -> Result<Option<Value>, Error> {
    if !matches!(item.plain(), Value::Dict(_)) {
        return Ok(None);
    }
    let entries = interpreter.mapping_items(item)?;
    Ok(match entries.as_slice() {
        [(key, merged)] if key.as_str() == Some("<") => Some(merged.clone()),
        _ => None,
    })
}

/// The `?if`, `?elif` and `?else` branches seen so far, which the next other key decides.
#[derive(Default)]
struct Conditional {
    branches: Vec<(Option<String>, Value)>,
}

impl Conditional {
    /// The value of the first branch whose condition holds, if any, and a fresh start.
    fn decide(
        &mut self,
        interpreter: &mut Interpreter,
        scope: &Rc<Scope>,
    ) -> Result<Option<Value>, Error> {
        let branches = std::mem::take(&mut self.branches);
        for (condition, body) in branches {
            let holds = match condition {
                Some(condition) => {
                    let result = evaluate(interpreter, &condition, scope)?;
                    interpreter.truthy(&result)?
                }
                None => true,
            };
            if holds {
                let rendered = value(interpreter, &body, scope)?;
                return Ok((!rendered.is_none()).then_some(rendered));
            }
        }
        Ok(None)
    }
}

/// A mapping: each part is a mapping of one key, a chosen branch or the results of a loop;
/// mappings merge, lists join, and a single other value stands for the whole mapping.
fn mapping(interpreter: &mut Interpreter, item: &Value, scope: &Rc<Scope>) -> Result<Value, Error> {
    let entries = interpreter.mapping_items(item)?;
    if entries.is_empty() {
        return Ok(Value::dict(Dict::default()));
    }
    let mut parts: Vec<Value> = Vec::new();
    let mut conditional = Conditional::default();
    for (key, body) in entries {
        let text = key.as_str().map(str::to_owned);
        match text.as_deref() {
            Some("__definitions__") => {
                let Value::List(statements) = body.plain() else {
                    return Err(workflow_error(
                        "__definitions__ keyword expects a list of Python statements",
                    ));
                };
                for statement in statements.borrow().iter() {
                    let code = interpreter.to_str(statement)?;
                    execute(interpreter, &code, scope)?;
                }
            }
            Some("__variables__") => {
                for (name, named) in interpreter.mapping_items(&body)? {
                    let name = interpreter.to_str(&name)?;
                    let named = match named.as_str() {
                        Some(text) if text.starts_with('?') => {
                            evaluate(interpreter, &text[1..], scope)?
                        }
                        _ => named,
                    };
                    scope.set(&name, named);
                }
            }
            Some(text) if text.starts_with("?for ") => {
                parts.extend(conditional.decide(interpreter, scope)?);
                parts.extend(repeat(interpreter, &text[5..], &body, scope)?);
            }
            Some(text) if text.starts_with("?if ") => {
                parts.extend(conditional.decide(interpreter, scope)?);
                conditional
                    .branches
                    .push((Some(text[4..].to_owned()), body));
            }
            Some(text) if text.starts_with("?elif ") => {
                if conditional.branches.is_empty() {
                    return Err(workflow_error("Unexpected elif: no if or elif before"));
                }
                conditional
                    .branches
                    .push((Some(text[6..].to_owned()), body));
            }
            Some("?else") => {
                if conditional.branches.is_empty() {
                    return Err(workflow_error("Unexpected else: no if or elif before"));
                }
                conditional.branches.push((None, body));
                parts.extend(conditional.decide(interpreter, scope)?);
            }
            _ => {
                parts.extend(conditional.decide(interpreter, scope)?);
                let key = value(interpreter, &key, scope)?;
                let body = value(interpreter, &body, scope)?;
                let mut part = Dict::default();
                part.entries.insert(Key::new(key)?, body);
                parts.push(Value::dict(part));
            }
        }
    }
    parts.extend(conditional.decide(interpreter, scope)?);
    combine(interpreter, parts)
}

/// Joins the parts of a mapping as yte does.
fn combine(interpreter: &mut Interpreter, parts: Vec<Value>) -> Result<Value, Error> {
    if parts
        .iter()
        .all(|part| matches!(part.plain(), Value::Dict(_)))
    {
        let mut result = Dict::default();
        for part in &parts {
            for (key, body) in interpreter.mapping_items(part)? {
                if key.as_str() == Some("<") {
                    if !matches!(body.plain(), Value::Dict(_)) {
                        return Err(workflow_error(
                            "Merge operator '<:' as key in a map must be followed by a map",
                        ));
                    }
                    for (key, merged) in interpreter.mapping_items(&body)? {
                        result.entries.insert(Key::new(key)?, merged);
                    }
                } else {
                    result.entries.insert(Key::new(key)?, body);
                }
            }
        }
        return Ok(Value::dict(result));
    }
    if parts
        .iter()
        .all(|part| matches!(part.plain(), Value::List(_)))
    {
        let mut result = Vec::new();
        for part in &parts {
            if let Value::List(items) = part.plain() {
                result.extend(items.borrow().iter().cloned());
            }
        }
        return Ok(Value::list(result));
    }
    match <[Value; 1]>::try_from(parts) {
        Ok([single]) => Ok(single),
        Err(_) => Err(workflow_error(
            "Conditional or for loop did not consistently return map or list.",
        )),
    }
}

/// `?for <names> in <expression>`: the body once for each item, with the names bound in a
/// scope of its own.
fn repeat(
    interpreter: &mut Interpreter,
    header: &str,
    body: &Value,
    scope: &Rc<Scope>,
) -> Result<Vec<Value>, Error> {
    let Some((target, iterable)) = header.split_once(" in ") else {
        return Err(workflow_error(format!("Invalid for loop: ?for {header}")));
    };
    let names: Vec<&str> = (target.trim().trim_start_matches('(').trim_end_matches(')'))
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .collect();
    let iterable = evaluate(interpreter, iterable, scope)?;
    let mut results = Vec::new();
    for item in interpreter.iterate(&iterable)? {
        // A scope of its own, which starts with the names of the scope around it.
        let inner = Scope::new(None);
        *inner.vars.borrow_mut() = scope.vars.borrow().clone();
        if let [name] = names.as_slice() {
            inner.set(name, item);
        } else {
            let values = interpreter.iterate(&item)?;
            if values.len() != names.len() {
                return Err(workflow_error(format!(
                    "cannot unpack {target} in ?for {header}"
                )));
            }
            for (name, item) in names.iter().zip(values) {
                inner.set(name, item);
            }
        }
        results.push(value(interpreter, body, &inner)?);
    }
    Ok(results)
}

/// The value of a Python expression in the scope of the template.
fn evaluate(
    interpreter: &mut Interpreter,
    expression: &str,
    scope: &Rc<Scope>,
) -> Result<Value, Error> {
    const RESULT: &str = "__yte_result__";
    execute(interpreter, &format!("{RESULT} = ({expression})"), scope)?;
    let result = scope.vars.borrow_mut().shift_remove(RESULT);
    Ok(result.unwrap_or(Value::None))
}

/// Runs Python statements in the scope of the template.
fn execute(interpreter: &mut Interpreter, code: &str, scope: &Rc<Scope>) -> Result<(), Error> {
    let parsed = snakemake_parser::parse(code)
        .map_err(|error| Error::new("SyntaxError", format!("{}: {code}", error.message)))?;
    let frame = Frame::module(scope.clone(), Source::new("<yte>", code));
    interpreter.run(&parsed.python, &frame)
}
