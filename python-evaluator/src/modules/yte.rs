//! A stand-in for yte, the YAML template engine that Snakemake depends on: `process_yaml`
//! renders a YAML document whose values may be Python expressions (`?expr`), whose keys may
//! be conditions (`?if`, `?elif`, `?else`) and loops (`?for x in y`), and which may define
//! functions (`__definitions__`) and variables (`__variables__`).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::any::Any;
use std::rc::Rc;

use super::function;
use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::value::{Args, Dict, Key, Native, Scope, Value};

/// `yte.SKIP`: a list item that renders to it is left out.
struct Skip;

impl Native for Skip {
    fn type_name(&self) -> &str {
        "object"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn yte_error(message: impl Into<String>) -> Error {
    Error::new("YteError", message.into())
}

/// A copy of the variables, which a loop or a branch may extend.
fn copied(scope: &Rc<Scope>) -> Rc<Scope> {
    let copy = Scope::new(None);
    for (name, value) in scope.vars.borrow().iter() {
        copy.set(name, value.clone());
    }
    copy
}

fn key_text(key: &Value) -> Option<&str> {
    key.as_str()
}

/// Renders a value: maps and lists item by item, and `?expr` texts by evaluating them.
fn render(interpreter: &mut Interpreter, value: &Value, scope: &Rc<Scope>) -> Result<Value, Error> {
    match value.plain() {
        Value::Dict(_) => render_map(interpreter, value, scope),
        Value::List(items) => {
            let items = items.borrow().clone();
            let mut result = Vec::new();
            for item in items {
                // `- <: [...]` merges a list into the list.
                if let Value::Dict(map) = item.plain()
                    && map.borrow().entries.len() == 1
                    && let Some(nested) = map.borrow().get("<").cloned()
                {
                    let value = render(interpreter, &nested, scope)?;
                    match value.plain() {
                        Value::List(inner) => result.extend(inner.borrow().iter().cloned()),
                        _ => {
                            return Err(yte_error(
                                "Merge operator '<:' within a list must be followed by a list",
                            ));
                        }
                    }
                    continue;
                }
                let rendered = render(interpreter, &item, scope)?;
                if rendered.as_native::<Skip>().is_none() {
                    result.push(rendered);
                }
            }
            Ok(Value::list(result))
        }
        Value::Str(text) if text.starts_with('?') => {
            let expression = text[1..].to_owned();
            interpreter.eval_text(&expression, scope)
        }
        _ => Ok(value.clone()),
    }
}

/// The `?if`, `?elif` and `?else` keys seen so far, which render once the chain ends.
#[derive(Default)]
struct Conditional {
    branches: Vec<(String, Value)>,
    otherwise: Option<Value>,
}

impl Conditional {
    fn flush(
        &mut self,
        interpreter: &mut Interpreter,
        scope: &Rc<Scope>,
        items: &mut Vec<Value>,
    ) -> Result<(), Error> {
        if self.branches.is_empty() {
            return Ok(());
        }
        let branches = std::mem::take(&mut self.branches);
        let otherwise = self.otherwise.take();
        for (condition, value) in branches {
            let holds = interpreter.eval_text(&condition, scope)?;
            if interpreter.truthy(&holds)? {
                let rendered = render(interpreter, &value, scope)?;
                if !rendered.is_none() {
                    items.push(rendered);
                }
                return Ok(());
            }
        }
        if let Some(value) = otherwise {
            let rendered = render(interpreter, &value, scope)?;
            if !rendered.is_none() {
                items.push(rendered);
            }
        }
        Ok(())
    }
}

/// Renders a map. Each key gives items: plain keys a one-entry map, conditions and loops
/// what their values render to. The items become one map, or one list, or the one item.
fn render_map(
    interpreter: &mut Interpreter,
    value: &Value,
    scope: &Rc<Scope>,
) -> Result<Value, Error> {
    let entries = interpreter.mapping_items(value)?;
    if entries.is_empty() {
        return Ok(Value::dict(Dict::default()));
    }
    let mut items: Vec<Value> = Vec::new();
    let mut conditional = Conditional::default();
    for (key, value) in entries {
        let text = key_text(&key).map(str::to_owned);
        match text.as_deref() {
            Some("__definitions__") => {
                let Value::List(statements) = value.plain() else {
                    return Err(yte_error(
                        "__definitions__ keyword expects a list of Python statements",
                    ));
                };
                let statements = statements.borrow().clone();
                for statement in statements {
                    let code = interpreter.to_str(&statement)?;
                    interpreter.exec_text("<yte definitions>", &code, scope)?;
                }
            }
            Some("__variables__") => {
                if !matches!(value.plain(), Value::Dict(_)) {
                    return Err(yte_error(
                        "__variables__ keyword expects a map of variable names and values",
                    ));
                }
                for (name, variable) in interpreter.mapping_items(&value)? {
                    let name = interpreter.to_str(&name)?;
                    let variable = match variable.as_str() {
                        Some(text) if text.starts_with('?') => {
                            interpreter.eval_text(&text[1..], scope)?
                        }
                        _ => variable,
                    };
                    scope.set(&name, variable);
                }
            }
            Some(text) if text.starts_with("?for ") && text.contains(" in ") => {
                conditional.flush(interpreter, scope, &mut items)?;
                let header = &text[5..];
                let (target, source) = header.split_once(" in ").unwrap_or((header, ""));
                let iterable = interpreter.eval_text(source, scope)?;
                for item in interpreter.iterate(&iterable)? {
                    let inner = copied(scope);
                    inner.set("_yte_item", item);
                    interpreter.exec_text(
                        "<yte loop>",
                        &format!("{target} = _yte_item"),
                        &inner,
                    )?;
                    items.push(render(interpreter, &value, &inner)?);
                }
            }
            Some(text) if text.starts_with("?if ") => {
                conditional.flush(interpreter, scope, &mut items)?;
                conditional.branches.push((text[4..].to_owned(), value));
            }
            Some(text) if text.starts_with("?elif ") => {
                if conditional.branches.is_empty() {
                    return Err(yte_error("Unexpected elif: no if or elif before"));
                }
                conditional.branches.push((text[6..].to_owned(), value));
            }
            Some("?else") => {
                if conditional.branches.is_empty() {
                    return Err(yte_error("Unexpected else: no if or elif before"));
                }
                conditional.otherwise = Some(value);
                conditional.flush(interpreter, scope, &mut items)?;
            }
            _ => {
                conditional.flush(interpreter, scope, &mut items)?;
                let key = render(interpreter, &key, scope)?;
                let value = render(interpreter, &value, scope)?;
                let mut dict = Dict::default();
                dict.entries.insert(Key::new(key)?, value);
                items.push(Value::dict(dict));
            }
        }
    }
    conditional.flush(interpreter, scope, &mut items)?;
    if items
        .iter()
        .all(|item| matches!(item.plain(), Value::Dict(_)))
    {
        let mut result = Dict::default();
        for item in &items {
            for (key, value) in interpreter.mapping_items(item)? {
                if key.as_str() == Some("<") {
                    // `<: {...}` merges a map into the map.
                    if !matches!(value.plain(), Value::Dict(_)) {
                        return Err(yte_error(
                            "Merge operator '<:' as key in a map must be followed by a map",
                        ));
                    }
                    for (key, value) in interpreter.mapping_items(&value)? {
                        result.entries.insert(Key::new(key)?, value);
                    }
                } else {
                    result.entries.insert(Key::new(key)?, value);
                }
            }
        }
        return Ok(Value::dict(result));
    }
    if items
        .iter()
        .all(|item| matches!(item.plain(), Value::List(_)))
    {
        let mut result = Vec::new();
        for item in &items {
            result.extend(interpreter.iterate(item)?);
        }
        return Ok(Value::list(result));
    }
    if items.len() == 1 {
        return Ok(items.remove(0));
    }
    Err(yte_error(
        "Conditional or for loop did not consistently return map or list.",
    ))
}

/// `process_yaml(file_or_str, outfile=None, variables=None, require_use_yte=False, ...)`.
fn process_yaml(interpreter: &mut Interpreter, mut args: Args) -> Result<Value, Error> {
    let source = args.take(0, "file_or_str").ok_or_else(|| {
        Error::type_error("process_yaml() missing 1 required argument: 'file_or_str'")
    })?;
    let outfile = args.take(1, "outfile").filter(|value| !value.is_none());
    let variables = args.take(2, "variables").filter(|value| !value.is_none());
    let require_use_yte = match args.take(3, "require_use_yte") {
        Some(value) => interpreter.truthy(&value)?,
        None => false,
    };
    if outfile.is_some() {
        return Err(Error::unsupported("yte.process_yaml with an outfile"));
    }
    let text = match source.as_str() {
        Some(text) => text.to_owned(),
        None => {
            let read = interpreter.get_attribute(&source, "read")?;
            let content = interpreter.call(&read, Args::default())?;
            interpreter.to_str(&content)?
        }
    };
    let document = super::data::load_yaml(&text)?;
    render_document(interpreter, document, variables.as_ref(), require_use_yte)
}

/// Renders an already loaded YAML document with the same semantics as `process_yaml`.
pub fn render_document(
    interpreter: &mut Interpreter,
    document: Value,
    variables: Option<&Value>,
    require_use_yte: bool,
) -> Result<Value, Error> {
    let scope = Scope::new(None);
    if let Some(variables) = variables {
        for (name, value) in interpreter.mapping_items(variables)? {
            scope.set(&interpreter.to_str(&name)?, value);
        }
    }
    scope.set("SKIP", skip_value());
    // `__use_yte__: true` asks for rendering when `require_use_yte` is given.
    let mut document = document;
    let mut use_yte = None;
    if let Value::Dict(map) = document.plain() {
        let mut copy = map.borrow().clone();
        use_yte = copy.entries.shift_remove(&Key::str("__use_yte__"));
        document = Value::dict(copy);
    }
    let marked = match &use_yte {
        Some(value) => interpreter.truthy(value)?,
        None => false,
    };
    if require_use_yte && !marked {
        return Ok(document);
    }
    render(interpreter, &document, &scope)
}

thread_local! {
    static SKIP: Value = Value::native(Skip);
}

fn skip_value() -> Value {
    SKIP.with(Value::clone)
}

pub fn yte() -> Vec<(&'static str, Value)> {
    vec![
        ("process_yaml", function("process_yaml", process_yaml)),
        ("SKIP", skip_value()),
    ]
}
