//! The functions that Snakemake puts into every Snakefile: file flags like `temp`,
//! `expand`, `glob_wildcards`, `multiext`, `lookup`, `branch` and the others.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;

use indexmap::IndexMap;
use python_evaluator::value::NativeFunction;
use python_evaluator::{Args, Dict, Error, Interpreter, Key, Native, Scope, Value};

use crate::objects::NamedList;
use crate::pattern;
use crate::workflow::{State, workflow_error};

/// A function made by a helper like `branch` or `expand` whose result depends on wildcards;
/// it states which parameters it takes, as Snakemake inspects input functions.
pub struct Deferred {
    pub parameters: Vec<String>,
    pub function: NativeFunction,
}

impl Native for Deferred {
    fn type_name(&self) -> &str {
        "function"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn instance_of(&self, class: &str) -> bool {
        class == "callable"
    }

    fn call(
        &self,
        interpreter: &mut Interpreter,
        _: &Value,
        args: Args,
    ) -> Result<Option<Value>, Error> {
        let function = self.function.clone();
        function(interpreter, args).map(Some)
    }
}

pub fn deferred(
    parameters: &[&str],
    function: impl Fn(&mut Interpreter, Args) -> Result<Value, Error> + 'static,
) -> Value {
    Value::native(Deferred {
        parameters: parameters.iter().map(|name| (*name).to_owned()).collect(),
        function: Rc::new(function),
    })
}

/// The parameter names of a callable and whether it takes `**kwargs`; `None` when they
/// are unknown, as for builtins.
pub fn parameters(value: &Value) -> Option<(Vec<String>, bool)> {
    match value.plain() {
        Value::Function(function) => {
            let parameters = function.code.parameters();
            let var_keyword = parameters.is_some_and(|parameters| parameters.kwarg.is_some());
            let mut names = function.code.parameter_names();
            if let Some(parameters) = parameters
                && let Some(kwarg) = &parameters.kwarg
            {
                names.retain(|name| name != kwarg.name.id.as_str());
            }
            Some((names, var_keyword))
        }
        Value::Method(method) => match &method.function {
            python_evaluator::value::MethodFunction::Value(function) => {
                let (mut names, var_keyword) = parameters(function)?;
                if !names.is_empty() {
                    names.remove(0);
                }
                Some((names, var_keyword))
            }
            python_evaluator::value::MethodFunction::Builtin(_) => None,
        },
        Value::Native(object) => object
            .as_any()
            .downcast_ref::<Deferred>()
            .map(|deferred| (deferred.parameters.clone(), false)),
        _ => None,
    }
}

/// Whether a value can serve as an input function.
pub fn is_callable(interpreter: &mut Interpreter, value: &Value) -> bool {
    match value.plain() {
        Value::Str(_) | Value::List(_) | Value::Tuple(_) | Value::Dict(_) => false,
        other => interpreter.callable(other),
    }
}

/// Calls an input function with wildcards and those of the auxiliary values that it takes,
/// as `apply_input_function` does; functions that return functions are called again.
pub fn call_input_function(
    interpreter: &mut Interpreter,
    function: &Value,
    wildcards: &Value,
    aux: &[(&str, Value)],
) -> Result<Value, Error> {
    let mut current = function.clone();
    for _ in 0..10 {
        let mut args = Args::new(vec![wildcards.clone()]);
        if let Some((names, var_keyword)) = parameters(&current) {
            for (name, value) in aux {
                if var_keyword || names.iter().any(|parameter| parameter == name) {
                    args.keywords.push(((*name).into(), value.clone()));
                }
            }
        }
        let result = interpreter.call(&current, args)?;
        let result = match result.plain() {
            Value::Iterator(_) => Value::list(interpreter.iterate(&result)?),
            _ => result,
        };
        if is_callable(interpreter, &result) {
            current = result;
            continue;
        }
        return Ok(result);
    }
    Err(workflow_error(
        "Evaluated 10 nested input functions (i.e. input functions that themselves return an input function.). More than 10 such nested evaluations are not allowed.",
    ))
}

/// Snakemake's `flag`: flags a value, or each item of a list.
pub fn flag(
    interpreter: &mut Interpreter,
    value: &Value,
    name: &str,
    flag_value: Value,
) -> Result<Value, Error> {
    match value {
        Value::Tagged(_) => Ok(value.with_tag(name, flag_value)),
        Value::List(_)
        | Value::Tuple(_)
        | Value::Set(_)
        | Value::FrozenSet(_)
        | Value::Iterator(_) => {
            let items = interpreter.iterate(value)?;
            let flagged = items
                .iter()
                .map(|item| flag(interpreter, item, name, flag_value.clone()))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Value::list(flagged))
        }
        Value::Native(object) if object.as_any().is::<NamedList>() => {
            let items = interpreter.iterate(value)?;
            let flagged = items
                .iter()
                .map(|item| flag(interpreter, item, name, flag_value.clone()))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Value::list(flagged))
        }
        Value::Native(object) if object.type_name() == "PosixPath" => {
            let text = interpreter.to_str(value)?;
            Ok(Value::str(text).with_tag(name, flag_value))
        }
        other => Ok(other.with_tag(name, flag_value)),
    }
}

fn flag_function(name: &'static str) -> Value {
    Value::builtin(name, move |interpreter, args| {
        let value = args
            .positional
            .first()
            .cloned()
            .ok_or_else(|| Error::type_error(format!("{name}() missing value")))?;
        if name == "directory" && value.tag("pipe").is_some() {
            return Err(Error::new(
                "SyntaxError",
                "Pipe and directory flags are mutually exclusive.",
            ));
        }
        let flag_name = if name == "temporary" { "temp" } else { name };
        flag(interpreter, &value, flag_name, Value::Bool(true))
    })
}

/// Parses `name=value` wildcard arguments of `expand` into lists of values.
fn expand_values(interpreter: &mut Interpreter, value: &Value) -> Result<Vec<Value>, Error> {
    Ok(match value.plain() {
        Value::Str(_) => vec![value.clone()],
        Value::Native(object) if object.type_name() == "PosixPath" => vec![value.clone()],
        Value::Native(object) if object.as_any().is::<NamedList>() => interpreter.iterate(value)?,
        Value::Native(object) if object.length().is_some() && !object.instance_of("tuple") => {
            interpreter.iterate(value)?
        }
        Value::List(_)
        | Value::Set(_)
        | Value::FrozenSet(_)
        | Value::Dict(_)
        | Value::Iterator(_)
        | Value::Range(_) => interpreter.iterate(value)?,
        Value::Tuple(_) => interpreter.iterate(value)?,
        Value::Native(_) => match interpreter.iterate(value) {
            Ok(items)
                if !value
                    .plain()
                    .as_native::<python_evaluator::modules::PathObject>()
                    .is_some() =>
            {
                items
            }
            _ => vec![value.clone()],
        },
        _ => vec![value.clone()],
    })
}

/// The names that a pattern refers to at the start of its fields, as `expand` finds them
/// with `{([^}\.[!:]+)`.
fn field_names(pattern: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = pattern;
    while let Some(start) = rest.find('{') {
        let after = &rest[start + 1..];
        let end = after.find(['}', '.', '[', '!', ':']).unwrap_or(after.len());
        if end > 0 {
            names.push(after[..end].to_owned());
        }
        rest = after;
    }
    names
}

/// `expand(patterns, [combinator], allow_missing=False, **wildcards)`.
pub fn expand(interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    let mut args = args;
    let allow_missing = match args.take_keyword("allow_missing") {
        Some(value) => interpreter.truthy(&value)?,
        None => false,
    };
    if args.positional.is_empty() {
        return Err(Error::type_error("expand() missing file patterns"));
    }
    let patterns_value = args.positional[0].clone();
    let combinator = args.positional.get(1).cloned();
    let patterns: Vec<Value> = match patterns_value.plain() {
        Value::Str(_) => vec![patterns_value.clone()],
        Value::Native(object) if object.type_name() == "PosixPath" => {
            vec![Value::str(interpreter.to_str(&patterns_value)?)]
        }
        _ => interpreter
            .iterate(&patterns_value)?
            .into_iter()
            .map(|item| match item.plain() {
                Value::Native(object) if object.type_name() == "PosixPath" => {
                    interpreter.to_str(&item).map(Value::str)
                }
                _ => Ok(item),
            })
            .collect::<Result<_, _>>()?,
    };
    for pattern in &patterns {
        if let Some(tags) = pattern.tags()
            && tags.keys().any(|name| &**name != "rule")
        {
            let flags: Vec<&str> = tags.keys().map(|name| &**name).collect();
            return Err(workflow_error(format!(
                "Flags ({}) in file pattern '{}' given to expand() are invalid. Flags (e.g. temp(), directory()) have to be applied outside of expand (e.g. 'temp(expand(\"plots/{{sample}}.pdf\", sample=SAMPLES))').",
                flags.join(", "),
                pattern.as_str().unwrap_or_default()
            )));
        }
    }
    let keywords = args.keywords.clone();
    let mut callables = Vec::new();
    for (name, value) in &keywords {
        let is_function = is_callable(interpreter, value)
            || matches!(value.plain(), Value::List(items) if items.borrow().iter().any(|item| matches!(item.plain(), Value::Function(_))));
        if is_function {
            callables.push(name.clone());
        }
    }
    if !callables.is_empty() {
        // Deferred: an input function that expands once wildcards are known.
        return Ok(deferred(
            &["wildcards", "input", "output", "threads", "resources"],
            move |interpreter, call_args| {
                let wildcards = call_args.positional.first().cloned().unwrap_or(Value::None);
                let aux: Vec<(&str, Value)> = Vec::new();
                let mut resolved = Vec::new();
                for (name, value) in &keywords {
                    if callables.contains(name) {
                        let result = match value.plain() {
                            Value::List(items) => {
                                let items = items.borrow().clone();
                                let mut results = Vec::new();
                                for item in items {
                                    if is_callable(interpreter, &item) {
                                        results.push(call_input_function(
                                            interpreter,
                                            &item,
                                            &wildcards,
                                            &aux,
                                        )?);
                                    } else {
                                        results.push(item);
                                    }
                                }
                                Value::list(results)
                            }
                            _ => call_input_function(interpreter, value, &wildcards, &aux)?,
                        };
                        resolved.push((name.clone(), result));
                    } else {
                        resolved.push((name.clone(), value.clone()));
                    }
                }
                do_expand(
                    interpreter,
                    &patterns,
                    combinator.as_ref(),
                    &resolved,
                    allow_missing,
                )
            },
        ));
    }
    do_expand(
        interpreter,
        &patterns,
        combinator.as_ref(),
        &keywords,
        allow_missing,
    )
}

fn do_expand(
    interpreter: &mut Interpreter,
    patterns: &[Value],
    combinator: Option<&Value>,
    keywords: &[(Rc<str>, Value)],
    allow_missing: bool,
) -> Result<Value, Error> {
    let product = match combinator {
        Some(combinator) => combinator.clone(),
        None => {
            let itertools = interpreter.import_module("itertools")?;
            interpreter.get_attribute(&itertools, "product")?
        }
    };
    let mut results = Vec::new();
    for pattern_value in patterns {
        let pattern = interpreter.to_str(pattern_value)?;
        let used = field_names(&pattern);
        // One list of (name, value) pairs per wildcard that the pattern uses.
        let mut lists = Vec::new();
        for (name, value) in keywords {
            if !used.iter().any(|used| **used == **name) {
                continue;
            }
            let values = expand_values(interpreter, value)?;
            lists.push(Value::list(
                values
                    .into_iter()
                    .map(|value| Value::tuple(vec![Value::Str(name.clone()), value]))
                    .collect(),
            ));
        }
        let combinations = interpreter.call(&product, Args::new(lists))?;
        for combination in interpreter.iterate(&combinations)? {
            let mut values: IndexMap<String, Value> = IndexMap::new();
            for pair in interpreter.iterate(&combination)? {
                let pair = interpreter.iterate(&pair)?;
                if let [name, value] = pair.as_slice() {
                    values.insert(name.as_str().unwrap_or_default().to_owned(), value.clone());
                }
            }
            let mut resolve = |_: &mut Interpreter, name: &str| -> Result<Value, Error> {
                match values.get(name) {
                    Some(value) => Ok(value.clone()),
                    None if allow_missing => Ok(Value::str(format!("{{{name}}}"))),
                    None => Err(workflow_error(format!(
                        "No values given for wildcard '{name}'."
                    ))),
                }
            };
            let mut plain = |interpreter: &mut Interpreter, value: &Value, spec: &str| {
                python_evaluator::format::format_value(interpreter, value, spec)
            };
            let text =
                python_evaluator::format::vformat(interpreter, &pattern, &mut resolve, &mut plain)?;
            results.push(crate::rule::retag(pattern_value, Value::str(text)));
        }
    }
    Ok(Value::list(results))
}

/// `multiext(prefix, *extensions, **named)`.
fn multiext(interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    let prefix = args
        .positional
        .first()
        .cloned()
        .ok_or_else(|| Error::type_error("multiext() missing prefix"))?;
    let prefix_text = python_evaluator::modules::path_text(interpreter, &prefix)?;
    let extensions = &args.positional[1..];
    if !extensions.is_empty() && !args.keywords.is_empty() {
        return Err(workflow_error(
            "multiext should be given with all named extensions or all not-named extensions, not a mix.",
        ));
    }
    let mut files = Vec::new();
    let mut add = |interpreter: &mut Interpreter,
                   name: Option<&str>,
                   extension: &Value|
     -> Result<(), Error> {
        let extension = interpreter.to_str(extension)?;
        if extension.contains(['/', '\\']) {
            return Err(workflow_error(
                "Extensions for multiext may not contain path delimiters (/,\\).",
            ));
        }
        let mut flag_value = Dict::default();
        flag_value.insert("prefix", Value::str(prefix_text.as_str()));
        flag_value.insert("name", name.map_or(Value::None, Value::str));
        let file = crate::rule::retag(&prefix, Value::str(format!("{prefix_text}{extension}")));
        files.push(file.with_tag("multiext", Value::dict(flag_value)));
        Ok(())
    };
    for extension in extensions {
        add(interpreter, None, extension)?;
    }
    for (name, extension) in &args.keywords {
        add(interpreter, Some(name), extension)?;
    }
    Ok(Value::list(files))
}
