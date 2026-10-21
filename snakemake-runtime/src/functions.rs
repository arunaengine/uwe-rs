//! The functions that Snakemake puts into every Snakefile: file flags like `temp`,
//! `expand`, `glob_wildcards`, `multiext`, `lookup`, `branch` and the others.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::any::Any;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::{Rc, Weak};

use indexmap::IndexMap;
use python_evaluator::modules::subprocess::called_process_error;
use python_evaluator::value::NativeFunction;
use python_evaluator::{Args, Command, Dict, Error, Interpreter, Key, Native, Scope, Value};

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
        // A named tuple, like a row that `lookup` gives, is one value.
        _ if python_evaluator::modules::collections::is_named_tuple(value) => vec![value.clone()],
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
        // The rule of a file and the path modification of a module are no flags.
        let flag = |name: &str| name != "rule" && name != "path_modified";
        if let Some(tags) = pattern.tags()
            && tags.keys().any(|name| flag(name))
        {
            let flags: Vec<&str> = tags
                .keys()
                .map(|name| &**name)
                .filter(|name| flag(name))
                .collect();
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

/// `glob_wildcards(pattern, files=None)`: the values of wildcards in existing files.
pub fn glob_wildcards(interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    let mut args = args;
    let files = args.take(1, "files").filter(|files| !files.is_none());
    args.take_keyword("followlinks");
    let pattern = args
        .positional
        .first()
        .cloned()
        .ok_or_else(|| Error::type_error("glob_wildcards() missing pattern"))?;
    let pattern = python_evaluator::modules::path_text(interpreter, &pattern)?;
    let pattern = normpath(&pattern);
    let first = pattern
        .find('{')
        .filter(|index| !pattern[index + 1..].starts_with('{'));
    let before = match first {
        Some(index) => &pattern[..index],
        None => &pattern[..],
    };
    let directory = match before.rfind('/') {
        Some(0) => "/".to_owned(),
        Some(index) => before[..index].to_owned(),
        None => ".".to_owned(),
    };
    let names = pattern::wildcard_names(&pattern);
    let matcher = pattern::Matcher::of(&pattern);
    if let Some(error) = matcher.error() {
        return Err(Error::new("re.error", error.to_owned()));
    }
    let candidates: Vec<String> = match files {
        Some(files) => interpreter
            .iterate(&files)?
            .iter()
            .map(|file| interpreter.to_str(file))
            .collect::<Result<_, _>>()?,
        None => walk(interpreter, &directory),
    };
    let mut columns: Vec<Vec<Value>> = vec![Vec::new(); names.len()];
    for candidate in candidates {
        if let Some(values) = matcher.matches(&candidate) {
            for (index, name) in names.iter().enumerate() {
                columns[index].push(Value::str(values.get(name).cloned().unwrap_or_default()));
            }
        }
    }
    let collections = interpreter.import_module("collections")?;
    let namedtuple = interpreter.get_attribute(&collections, "namedtuple")?;
    let class = interpreter.call(
        &namedtuple,
        Args::new(vec![
            Value::str("Wildcards"),
            Value::list(names.iter().map(|name| Value::str(name.as_str())).collect()),
        ]),
    )?;
    interpreter.call(
        &class,
        Args::new(columns.into_iter().map(Value::list).collect()),
    )
}

/// `os.path.normpath`.
pub(crate) fn normpath(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." if parts.last().is_some_and(|last| *last != "..") => {
                parts.pop();
            }
            ".." if absolute => {}
            other => parts.push(other),
        }
    }
    let joined = parts.join("/");
    match (absolute, joined.is_empty()) {
        (true, _) => format!("/{joined}"),
        (false, true) => ".".into(),
        (false, false) => joined,
    }
}

/// Every file and folder below a folder, as `os.walk` finds them, with paths spelled from
/// the folder given.
fn walk(interpreter: &mut Interpreter, top: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![top.to_owned()];
    while let Some(directory) = pending.pop() {
        let resolved = interpreter.host.resolve(&directory);
        let Ok(names) = interpreter.host.list(&resolved) else {
            continue;
        };
        for name in names {
            let path = if directory == "." {
                name.clone()
            } else {
                format!("{}/{name}", directory.trim_end_matches('/'))
            };
            let child = resolved.join(&name);
            if interpreter
                .host
                .stat(&child)
                .is_some_and(|stat| stat.kind == python_evaluator::FileKind::Directory)
            {
                pending.push(path.clone());
            }
            found.push(normpath(&path));
        }
    }
    found
}

/// `snakemake.utils.listfiles(pattern, restriction=None, omit_value=None)`: each existing
/// file or folder that matches the pattern, with its wildcard values, in the order of a
/// walk that gives the files of a folder before its folders.
pub fn listfiles(interpreter: &mut Interpreter, mut args: Args) -> Result<Value, Error> {
    let pattern = args
        .take(0, "pattern")
        .ok_or_else(|| Error::type_error("listfiles() missing pattern"))?;
    let restriction = args.take(1, "restriction").filter(|value| !value.is_none());
    let omit = args.take(2, "omit_value").filter(|value| !value.is_none());
    let pattern = normpath(&python_evaluator::modules::path_text(
        interpreter,
        &pattern,
    )?);
    let first = (pattern.match_indices('{'))
        .find(|(index, _)| !pattern[index + 1..].starts_with('{'))
        .map(|(index, _)| index);
    let before = first.map_or(&pattern[..], |index| &pattern[..index]);
    let top = match before.rfind('/') {
        Some(0) => "/".to_owned(),
        Some(index) => before[..index].to_owned(),
        None => ".".to_owned(),
    };
    let matcher = pattern::Matcher::of(&pattern);
    if let Some(error) = matcher.error() {
        return Err(Error::new("re.error", error.to_owned()));
    }
    let restriction = match &restriction {
        Some(mapping) => interpreter.mapping_items(mapping)?,
        None => Vec::new(),
    };
    let mut found = Vec::new();
    let mut pending = vec![top];
    while let Some(folder) = pending.pop() {
        let resolved = interpreter.host.resolve(&folder);
        let Ok(names) = interpreter.host.list(&resolved) else {
            continue;
        };
        let (mut files, mut folders) = (Vec::new(), Vec::new());
        for name in names {
            let directory = (interpreter.host.stat(&resolved.join(&name)))
                .is_some_and(|stat| stat.kind == python_evaluator::FileKind::Directory);
            let path = match folder.as_str() {
                "." => name,
                _ => normpath(&format!("{}/{name}", folder.trim_end_matches('/'))),
            };
            match directory {
                true => folders.push(path),
                false => files.push(path),
            }
        }
        for path in files.iter().chain(&folders) {
            let Some(values) = matcher.matches(path) else {
                continue;
            };
            let mut valid = true;
            for (name, wanted) in &restriction {
                let Some(omit) = &omit else {
                    return Err(Error::type_error(
                        "'in <string>' requires string as left operand, not NoneType",
                    ));
                };
                let (name, wanted) = (interpreter.to_str(name)?, interpreter.to_str(wanted)?);
                let omitted = wanted.contains(&interpreter.to_str(omit)?);
                if !omitted && values.get(&name) != Some(&wanted) {
                    valid = false;
                }
            }
            if valid {
                let wildcards = NamedList::wildcards(&values);
                found.push(Value::tuple(vec![Value::str(path.as_str()), wildcards]));
            }
        }
        // `os.walk` goes into the folders in order after the folder itself.
        pending.extend(folders.into_iter().rev());
    }
    Ok(Value::iterator(found))
}

/// `branch(condition, then=None, otherwise=None, cases=None)`.
fn branch(interpreter: &mut Interpreter, mut args: Args) -> Result<Value, Error> {
    let condition = args.take(0, "condition").unwrap_or(Value::None);
    let then = args.take(1, "then").unwrap_or(Value::None);
    let otherwise = args.take(2, "otherwise").unwrap_or(Value::None);
    let cases = args.take(3, "cases").filter(|cases| !cases.is_none());
    if cases.is_some() && !then.is_none() {
        return Err(Error::value_error(
            "Cannot use 'cases' together with 'then' in 'branch' function.",
        ));
    }
    let any_callable = [&condition, &then, &otherwise]
        .into_iter()
        .any(|value| is_callable(interpreter, value));
    let decide = move |interpreter: &mut Interpreter, wildcards: &Value| -> Result<Value, Error> {
        let resolve = |interpreter: &mut Interpreter, value: &Value| -> Result<Value, Error> {
            let value = if is_callable(interpreter, value) {
                interpreter.call(value, Args::new(vec![wildcards.clone()]))?
            } else {
                value.clone()
            };
            // `value or []`
            Ok(if interpreter.truthy(&value)? {
                value
            } else {
                Value::list(Vec::new())
            })
        };
        match &cases {
            Some(cases) => {
                let key = resolve(interpreter, &condition)?;
                match interpreter.get_item(cases, &key) {
                    Ok(selected) => resolve(interpreter, &selected),
                    Err(error) if error.is("KeyError") && !otherwise.is_none() => {
                        resolve(interpreter, &otherwise)
                    }
                    Err(error) => Err(error),
                }
            }
            None => {
                let truth = resolve(interpreter, &condition)?;
                if interpreter.truthy(&truth)? {
                    resolve(interpreter, &then)
                } else {
                    resolve(interpreter, &otherwise)
                }
            }
        }
    };
    if any_callable {
        return Ok(deferred(&["wildcards"], move |interpreter, args| {
            let wildcards = args.positional.first().cloned().unwrap_or(Value::None);
            decide(interpreter, &wildcards)
        }));
    }
    decide(interpreter, &Value::None)
}

/// `evaluate(expression)`: an input function that evaluates Python text with the wildcards
/// filled in as reprs.
fn evaluate(_: &mut Interpreter, args: Args) -> Result<Value, Error> {
    let expression = args
        .positional
        .first()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default();
    Ok(deferred(&["wildcards"], move |interpreter, args| {
        let wildcards = args.positional.first().cloned().unwrap_or(Value::None);
        let mut values = Vec::new();
        if let Some(list) = wildcards.as_native::<NamedList>() {
            for name in list.keys() {
                let value = list.named(&name).unwrap_or(Value::None);
                values.push((name.into(), Value::str(interpreter.repr(&value)?)));
            }
        }
        let text = python_evaluator::format::format_method(
            interpreter,
            &expression,
            &Args {
                positional: Vec::new(),
                keywords: values,
            },
        )?;
        let globals = interpreter.new_globals("snakemake.ioutils", None);
        interpreter.eval_text(&text, &globals)
    }))
}

/// `subpath(path_or_function, strip_suffix=None, with_suffix=None, basename=False,
/// parent=False, ancestor=None)`.
fn subpath(interpreter: &mut Interpreter, mut args: Args) -> Result<Value, Error> {
    let value = args.take(0, "path_or_func").unwrap_or(Value::None);
    let strip_suffix = args
        .take_keyword("strip_suffix")
        .and_then(|value| value.as_str().map(str::to_owned));
    let with_suffix = args
        .take_keyword("with_suffix")
        .and_then(|value| value.as_str().map(str::to_owned));
    let basename = args
        .take_keyword("basename")
        .is_some_and(|value| matches!(value.plain(), Value::Bool(true)));
    let parent = args
        .take_keyword("parent")
        .is_some_and(|value| matches!(value.plain(), Value::Bool(true)));
    let ancestor = args
        .take_keyword("ancestor")
        .and_then(|value| value.as_int());
    let apply = move |interpreter: &mut Interpreter, value: &Value| -> Result<Value, Error> {
        let mut path = python_evaluator::modules::path_text(interpreter, value)?;
        if let Some(suffix) = &strip_suffix {
            path = path
                .strip_suffix(suffix.as_str())
                .ok_or_else(|| {
                    Error::value_error(format!(
                        "Path {path} does not end with the specified suffix {suffix}"
                    ))
                })?
                .to_owned();
        }
        if let Some(suffix) = &with_suffix {
            path.push_str(suffix);
        }
        let dirname = |path: &str| -> String {
            match path.rfind('/') {
                Some(0) => "/".into(),
                Some(index) => path[..index].to_owned(),
                None => ".".into(),
            }
        };
        if basename {
            path = path.rsplit('/').next().unwrap_or_default().to_owned();
        } else if parent {
            path = dirname(&path);
        } else if let Some(count) = ancestor {
            for _ in 0..count {
                path = dirname(&path);
                if path == "." {
                    break;
                }
            }
        }
        Ok(Value::str(path))
    };
    if is_callable(interpreter, &value) {
        let names = parameters(&value)
            .map(|(names, _)| names)
            .unwrap_or_else(|| vec!["wildcards".into()]);
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        return Ok(deferred(&names, move |interpreter, args| {
            let result = interpreter.call(&value, args)?;
            apply(interpreter, &result)
        }));
    }
    apply(interpreter, &value)
}

/// `lookup(query=None, dpath=None, cols=None, is_nrows=None, within=None, default=...)`.
fn lookup(interpreter: &mut Interpreter, mut args: Args) -> Result<Value, Error> {
    let mut query = args.take(0, "query").filter(|value| !value.is_none());
    let mut dpath = args.take(1, "dpath").filter(|value| !value.is_none());
    let cols = args.take_keyword("cols").filter(|value| !value.is_none());
    let is_nrows = args
        .take_keyword("is_nrows")
        .filter(|value| !value.is_none());
    let within = args.take_keyword("within").filter(|value| !value.is_none());
    let default = args.take_keyword("default");
    let namespace = args.keywords.clone();
    let Some(within) = within else {
        return Err(lookup_error(
            "Must provide a dataframe, series, or mapping to search within.",
        ));
    };
    let mapping = matches!(within.plain(), Value::Dict(_));
    if query.is_some() && mapping {
        dpath = query.take();
    }
    let (expression, is_query) = match (query, dpath) {
        (Some(query), None) => (query, true),
        (None, Some(dpath)) => (dpath, false),
        _ => {
            return Err(lookup_error(
                "Must provide either a query or dpath parameter.",
            ));
        }
    };
    let apply = move |interpreter: &mut Interpreter,
                      expression: &str,
                      namespace: &[(Rc<str>, Value)]|
          -> Result<Value, Error> {
        if is_query {
            let cols = match &cols {
                Some(cols) => Some(format_with(interpreter, cols, namespace)?),
                None => None,
            };
            python_evaluator::pandas::lookup_query(
                interpreter,
                &within,
                expression,
                cols,
                is_nrows.clone(),
                default.clone(),
            )
        } else {
            match dpath_get(interpreter, &within, expression) {
                Ok(value) => Ok(value),
                Err(_) if default.is_some() => Ok(default.clone().unwrap_or(Value::None)),
                Err(error) => Err(error),
            }
        }
    };
    let text = expression.as_str().map(str::to_owned);
    let needs_wildcards = is_callable(interpreter, &expression)
        || text.as_ref().is_some_and(|text| {
            pattern::wildcard_names(text)
                .iter()
                .any(|name| !namespace.iter().any(|(key, _)| **key == **name))
        })
        || namespace
            .iter()
            .any(|(_, value)| matches!(value.plain(), Value::Function(_)));
    if needs_wildcards {
        return Ok(deferred(&["wildcards"], move |interpreter, call_args| {
            let wildcards = call_args.positional.first().cloned().unwrap_or(Value::None);
            let mut resolved: Vec<(Rc<str>, Value)> = Vec::new();
            for (name, value) in &namespace {
                let value = if is_callable(interpreter, value) {
                    interpreter.call(value, Args::new(vec![wildcards.clone()]))?
                } else {
                    value.clone()
                };
                resolved.push((name.clone(), value));
            }
            if let Some(list) = wildcards.as_native::<NamedList>() {
                for name in list.keys() {
                    if !resolved.iter().any(|(key, _)| **key == *name) {
                        resolved.push((
                            name.as_str().into(),
                            list.named(&name).unwrap_or(Value::None),
                        ));
                    }
                }
            }
            let expression = if is_callable(interpreter, &expression) {
                interpreter.call(&expression, Args::new(vec![wildcards.clone()]))?
            } else {
                expression.clone()
            };
            let formatted = format_with(interpreter, &expression, &resolved)?;
            let formatted = formatted.as_str().unwrap_or_default().to_owned();
            apply(interpreter, &formatted, &resolved)
        }));
    }
    let formatted = if namespace.is_empty() {
        expression.clone()
    } else {
        format_with(interpreter, &expression, &namespace)?
    };
    let formatted = interpreter.to_str(&formatted)?;
    apply(interpreter, &formatted, &namespace)
}

fn lookup_error(message: &str) -> Error {
    let class = python_evaluator::types::library_exception(
        "LookupError",
        "snakemake.exceptions",
        "WorkflowError",
    );
    Error::from_value(python_evaluator::error::exception(
        &class,
        vec![Value::str(message)],
    ))
}

/// Formats text or each text of a list with names, as `snakemake.utils.format` does.
fn format_with(
    interpreter: &mut Interpreter,
    value: &Value,
    namespace: &[(Rc<str>, Value)],
) -> Result<Value, Error> {
    match value.plain() {
        Value::Str(text) => {
            let args = Args {
                positional: Vec::new(),
                keywords: namespace.to_vec(),
            };
            Ok(Value::str(crate::shell::format_command(
                interpreter,
                text,
                &args,
                None,
            )?))
        }
        Value::List(items) => {
            let items = items.borrow().clone();
            let formatted = items
                .iter()
                .map(|item| format_with(interpreter, item, namespace))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Value::list(formatted))
        }
        other => Ok(other.clone()),
    }
}

/// `dpath.get` on nested mappings with paths like `a/b/0`; `*` and `**` segments give the
/// values of all matches, as `dpath.values` does.
fn dpath_get(interpreter: &mut Interpreter, within: &Value, path: &str) -> Result<Value, Error> {
    let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    let mut current = vec![within.clone()];
    let mut glob = false;
    for part in parts {
        let mut next = Vec::new();
        for value in &current {
            if part == "*" || part.contains(['*', '?', '[']) {
                glob = true;
                let regex = fancy_regex::Regex::new(&format!(
                    "^{}",
                    python_evaluator::modules::glob_regex(part)
                ))
                .map_err(|error| Error::value_error(error.to_string()))?;
                for (key, item) in interpreter.mapping_items(value).unwrap_or_default() {
                    let key_text = interpreter.to_str(&key)?;
                    if regex.is_match(&key_text).unwrap_or(false) {
                        next.push(item);
                    }
                }
                continue;
            }
            let key = match value.plain() {
                Value::List(_) | Value::Tuple(_) => part
                    .parse::<i64>()
                    .map(Value::Int)
                    .unwrap_or(Value::str(part)),
                _ => Value::str(part),
            };
            match interpreter.get_item(value, &key) {
                Ok(item) => next.push(item),
                Err(_) => {
                    if let Ok(number) = part.parse::<i64>()
                        && let Ok(item) = interpreter.get_item(value, &Value::Int(number))
                    {
                        next.push(item);
                    }
                }
            }
        }
        current = next;
    }
    if glob {
        return Ok(Value::list(current));
    }
    match current.len() {
        1 => Ok(current.remove(0)),
        0 => Err(lookup_error(&format!("Dpath {path} not found."))),
        _ => Ok(Value::list(current)),
    }
}

/// `input`, `output`, `params` and `resources` used outside of rule bodies: attribute and
/// item access that is resolved per job, like Snakemake's `RuleItemProxy`.
pub struct RuleItemProxy(pub &'static str);

impl Native for RuleItemProxy {
    fn type_name(&self) -> &str {
        "RuleItemProxy"
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
        let kind = self.0;
        let key = name.to_owned();
        Ok(Some(deferred(
            &["wildcards", kind],
            move |interpreter, mut args| {
                let item = args.take_keyword(kind).unwrap_or(Value::None);
                interpreter.get_attribute(&item, &key)
            },
        )))
    }

    fn get_item(
        &self,
        _: &mut Interpreter,
        _: &Value,
        key: &Value,
    ) -> Result<Option<Value>, Error> {
        let kind = self.0;
        let key = key.clone();
        Ok(Some(deferred(
            &["wildcards", kind],
            move |interpreter, mut args| {
                let item = args.take_keyword(kind).unwrap_or(Value::None);
                interpreter.get_item(&item, &key)
            },
        )))
    }
}

/// The `shell` global: the settings of shell commands, and `shell(cmd)`, which runs a command
/// while the workflow is planned. The host runs it as a task (decision 11).
struct Shell {
    state: Rc<RefCell<State>>,
    /// The globals of the Snakefile, whose names the command may use.
    globals: Weak<Scope>,
}

impl Native for Shell {
    fn type_name(&self) -> &str {
        "shell"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn call(
        &self,
        interpreter: &mut Interpreter,
        _: &Value,
        mut args: Args,
    ) -> Result<Option<Value>, Error> {
        let template = args
            .take(0, "cmd")
            .ok_or_else(|| Error::type_error("shell() missing 1 required argument: 'cmd'"))?;
        let mut flag = |name: &str| -> Result<bool, Error> {
            match args.take_keyword(name) {
                Some(value) => interpreter.truthy(&value),
                None => Ok(false),
            }
        };
        let iterable = flag("iterable")?;
        let read = flag("read")?;
        args.take_keyword("bench_record");
        let template = interpreter.to_str(&template)?;
        let globals = self.globals.upgrade();
        // Names of the calling function come after the keyword arguments, as in Snakemake.
        if let Some(locals) = interpreter.caller_locals.take()
            && !globals
                .as_ref()
                .is_some_and(|globals| Rc::ptr_eq(globals, &locals))
        {
            for (name, value) in locals.vars.borrow().iter() {
                args.keywords.push((name.clone(), value.clone()));
            }
        }
        let formatted =
            crate::shell::format_command(interpreter, &template, &args, globals.as_ref())?;
        let argv = crate::shell::command_line(&self.state.borrow(), formatted.trim());
        let text = argv.last().cloned().unwrap_or_default();
        let command = Command {
            argv,
            cwd: interpreter.host.cwd(),
        };
        let outcome = interpreter.host.run_command(&command)?;
        if outcome.exit_code != 0 {
            return Err(called_process_error(
                interpreter,
                &Value::str(text),
                &outcome,
                true,
            )?);
        }
        let stdout = String::from_utf8_lossy(&outcome.stdout).into_owned();
        Ok(Some(if iterable {
            Value::iterator(stdout.lines().map(Value::str).collect::<Vec<_>>())
        } else if read {
            Value::str(stdout)
        } else {
            if !stdout.is_empty() {
                interpreter.host.print(&stdout);
            }
            Value::None
        }))
    }

    fn instance_of(&self, class: &str) -> bool {
        class == "callable"
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        _: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        let state = self.state.clone();
        Ok(Some(match name {
            "prefix" | "suffix" | "executable" => Value::builtin(name, {
                let which = name.to_owned();
                move |interpreter, args| {
                    let value = match args.positional.first() {
                        Some(value) if !value.is_none() => Some(interpreter.to_str(value)?),
                        _ => None,
                    };
                    let mut state = state.borrow_mut();
                    match which.as_str() {
                        "prefix" => state.shell_prefix = value,
                        "suffix" => state.shell_suffix = value.unwrap_or_default(),
                        _ => state.shell_executable = value,
                    }
                    Ok(Value::None)
                }
            }),
            "conda_block_conflicting_envvars" => Value::Bool(true),
            _ => return Ok(None),
        }))
    }

    fn set_attribute(&self, _: &mut Interpreter, _: &str, _: Value) -> Result<bool, Error> {
        Ok(true)
    }
}

/// `scatter` and `gather`: `scatter.<name>(pattern)` expands a pattern over the scatter
/// items of `scattergather:`.
pub struct ScatterGather(pub Rc<RefCell<State>>);

impl Native for ScatterGather {
    fn type_name(&self) -> &str {
        "Scatter"
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
        let Some(count) = self.0.borrow().scatter.get(name).copied() else {
            return Ok(None);
        };
        Ok(Some(Value::builtin(name, move |interpreter, args| {
            let items: Vec<Value> = (1..=count)
                .map(|index| Value::str(format!("{index}-of-{count}")))
                .collect();
            let mut expand_args = Args::new(args.positional.clone());
            expand_args.keywords = args.keywords.clone();
            expand_args
                .keywords
                .push(("scatteritem".into(), Value::list(items)));
            expand(interpreter, expand_args)
        })))
    }
}

/// A workflow file in a git repository: on GitHub or GitLab, given by `github(...)` or
/// `gitlab(...)`, or on this machine, given by `gitfile(...)`.
pub struct RemoteSource {
    pub url: String,
    /// The local copy of a file on the web, which is downloaded when it is read.
    pub local: Option<PathBuf>,
    /// The repository, reference and path of `gitfile(...)`.
    pub git: Option<(String, String, String)>,
}

impl Native for RemoteSource {
    fn type_name(&self) -> &str {
        "HostingProviderFile"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn to_str(&self, _: &mut Interpreter) -> Result<Option<String>, Error> {
        Ok(Some(self.url.clone()))
    }
}

fn source_error(message: &str) -> Error {
    Error::new("SourceFileError", message)
}

/// `github(repo, path, tag=..., branch=..., commit=..., host=...)`, the same for `gitlab`,
/// and `gitfile(repo_path, path, tag=..., ref=..., commit=...)`, checked as in Snakemake.
fn hosted(provider: &'static str, state: Rc<RefCell<State>>) -> Value {
    Value::builtin(provider, move |interpreter, mut args| {
        let Some(repository) = args.take(0, "repo") else {
            return Err(source_error("repo must be given"));
        };
        let repository = interpreter.to_str(&repository)?;
        let Some(path) = args.take(1, "path") else {
            return Err(source_error("path must be given"));
        };
        let path = interpreter.to_str(&path)?;
        // `gitfile` is Snakemake's `LocalGitFile(repo_path, path, tag, ref, commit)`; the
        // others take a branch instead of a ref.
        let keys = match provider {
            "gitfile" => ["tag", "ref", "commit"],
            _ => ["tag", "branch", "commit"],
        };
        let mut references = Vec::new();
        for (index, key) in keys.into_iter().enumerate() {
            if let Some(value) = args.take(index + 2, key)
                && !value.is_none()
            {
                references.push(interpreter.to_str(&value)?);
            }
        }
        if provider == "gitfile" {
            args.no_keywords("LocalGitFile.__init__")?;
        }
        let [reference] = references.as_slice() else {
            return Err(source_error(
                "exactly one of tag, branch, or commit must be specified.",
            ));
        };
        if provider == "gitfile" {
            return Ok(Value::native(RemoteSource {
                url: format!("git+file://{repository}/{path}@{reference}"),
                local: None,
                git: Some((repository, reference.clone(), path)),
            }));
        }
        let (owner, name) = repository.split_once('/').unwrap_or_default();
        if owner.is_empty() || name.is_empty() {
            return Err(source_error(
                "repo {} is not a valid repo specification (must be given as owner/name).",
            ));
        }
        let host = match args.take_keyword("host") {
            Some(host) if !host.is_none() => interpreter.to_str(&host)?,
            _ => format!("{provider}.com"),
        };
        if host.starts_with("https://") || host.starts_with("http://") {
            return Err(workflow_error(format!(
                "host must be given as domain name without protocol prefix (e.g. github.com, but found {host})  for git-hosted source file {path} in repo {repository}."
            )));
        }
        let mirrors = state.borrow().mirrors.clone();
        let local = mirrors.hosted(provider, &host, &repository, reference, &path);
        let url = mirrors.address(&local).unwrap_or_default();
        Ok(Value::native(RemoteSource {
            url,
            local: Some(local),
            git: None,
        }))
    })
}

/// Puts the Snakemake names into the globals of a Snakefile.
pub fn install(globals: &Rc<Scope>, state: &Rc<RefCell<State>>) {
    for name in [
        "temp",
        "temporary",
        "protected",
        "directory",
        "ancient",
        "touch",
        "pipe",
        "service",
        "unpack",
        "local",
        "update",
        "before_update",
        "checkpoint_target",
    ] {
        globals.set(name, flag_function(name));
    }
    globals.set("expand", Value::builtin("expand", expand));
    globals.set("collect", Value::builtin("collect", expand));
    globals.set(
        "glob_wildcards",
        Value::builtin("glob_wildcards", glob_wildcards),
    );
    globals.set("multiext", Value::builtin("multiext", multiext));
    globals.set("branch", Value::builtin("branch", branch));
    globals.set("evaluate", Value::builtin("evaluate", evaluate));
    globals.set("subpath", Value::builtin("subpath", subpath));
    globals.set("lookup", Value::builtin("lookup", lookup));
    globals.set(
        "ensure",
        Value::builtin("ensure", |interpreter, mut args| {
            let value = args.positional.first().cloned().unwrap_or(Value::None);
            let mut options = Dict::default();
            for key in ["non_empty", "sha256", "md5", "sha1"] {
                options.insert(key, args.take_keyword(key).unwrap_or(Value::None));
            }
            flag(interpreter, &value, "ensure", Value::dict(options))
        }),
    );
    globals.set(
        "repeat",
        Value::builtin("repeat", |interpreter, args| {
            args.expect("repeat", 2, 2)?;
            flag(
                interpreter,
                &args.positional[0],
                "repeat",
                args.positional[1].clone(),
            )
        }),
    );
    globals.set(
        "report",
        Value::builtin("report", |interpreter, args| {
            let value = args.positional.first().cloned().unwrap_or(Value::None);
            flag(interpreter, &value, "report", Value::Bool(true))
        }),
    );
    globals.set(
        "from_queue",
        // The inputs from a queue that code fills. Threads that fill it ran when they
        // started, so the queue already holds all items up to the sentinel.
        Value::builtin("from_queue", |_, mut args| {
            let queue = args.take(0, "queue").unwrap_or(Value::None);
            let sentinel = args.take(1, "finish_sentinel").unwrap_or(Value::None);
            match python_evaluator::modules::system::drain_queue(&queue, &sentinel) {
                Some(items) => Ok(Value::list(items)),
                None => Err(Error::type_error("from_queue() needs a queue.Queue")),
            }
        }),
    );
    globals.set(
        "flag",
        Value::builtin("flag", |interpreter, args| {
            args.expect("flag", 2, 3)?;
            let name = args.positional[1].as_str().unwrap_or_default().to_owned();
            let value = args.positional.get(2).cloned().unwrap_or(Value::Bool(true));
            flag(interpreter, &args.positional[0], &name, value)
        }),
    );
    globals.set(
        "exists",
        Value::builtin("exists", |interpreter, args| {
            args.expect("exists", 1, 1)?;
            let path = python_evaluator::modules::path_text(interpreter, &args.positional[0])?;
            let resolved = interpreter.host.resolve(&path);
            Ok(Value::Bool(interpreter.host.stat(&resolved).is_some()))
        }),
    );
    crate::ioutils::install(globals);
    for (name, kind) in [
        ("input", "input"),
        ("output", "output"),
        ("params", "params"),
        ("resources", "resources"),
    ] {
        globals.set(name, Value::native(RuleItemProxy(kind)));
    }
    globals.set(
        "threads",
        deferred(&["wildcards", "threads"], |_, mut args| {
            Ok(args.take_keyword("threads").unwrap_or(Value::Int(1)))
        }),
    );
    globals.set("github", hosted("github", state.clone()));
    globals.set("gitlab", hosted("gitlab", state.clone()));
    globals.set("gitfile", hosted("gitfile", state.clone()));
    globals.set(
        "shell",
        Value::native(Shell {
            state: state.clone(),
            globals: Rc::downgrade(globals),
        }),
    );
    globals.set("scatter", Value::native(ScatterGather(state.clone())));
    globals.set("gather", Value::native(ScatterGather(state.clone())));
    globals.set(
        "rules",
        Value::native(crate::objects::RulesObject::new(state.clone())),
    );
    globals.set(
        "checkpoints",
        Value::native(crate::objects::CheckpointsObject::new(state.clone())),
    );
    globals.set(
        "workflow",
        Value::native(crate::objects::WorkflowObject(state.clone())),
    );
    globals.set(
        "storage",
        Value::native(crate::storage::StorageRegistry {
            state: Rc::downgrade(state),
        }),
    );
    let mut access = Dict::default();
    for name in ["random", "sequential", "multi"] {
        access.insert(
            name,
            Value::builtin(name, |_, args| {
                Ok(args.positional.first().cloned().unwrap_or(Value::None))
            }),
        );
    }
    globals.set(
        "access",
        Value::native(python_evaluator::builtins::Record(
            RefCell::new(access),
            "AccessPatternFactory",
        )),
    );
    let _ = Key::str;
}
