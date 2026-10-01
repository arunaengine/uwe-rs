//! Provides Snakemake file-to-parameter helpers and usable-path selection.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};
use std::rc::Rc;

use python_evaluator::modules::{PathObject, path_text};
use python_evaluator::{Args, Error, Interpreter, Scope, Value};

use crate::functions::{Deferred, RuleItemProxy, deferred, is_callable, parameters};

pub fn install(globals: &Rc<Scope>) {
    globals.set(
        "prepend_param",
        Value::builtin("prepend_param", prepend_param),
    );
    globals.set("as_py_module", Value::builtin("as_py_module", as_py_module));
    globals.set("parse_input", Value::builtin("parse_input", parse_input));
    globals.set("flatten", Value::builtin("flatten", flatten));
    globals.set(
        "extract_checksum",
        Value::builtin("extract_checksum", extract_checksum),
    );
    for (name, is_directory, creatable) in [
        ("choose_file", false, None),
        ("choose_folder", true, None),
        ("choose_tmp", true, Some(true)),
    ] {
        globals.set(
            name,
            Value::builtin(name, move |interpreter, args| {
                choose(interpreter, args, name, is_directory, creatable)
            }),
        );
    }
}

/// `flatten(list_of_lists)`: the items of nested lists as texts, in order.
fn flatten(interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    fn walk(
        interpreter: &mut Interpreter,
        value: &Value,
        into: &mut Vec<Value>,
    ) -> Result<(), Error> {
        for item in interpreter.iterate(value)? {
            match item.plain() {
                Value::List(_) => walk(interpreter, &item, into)?,
                _ => into.push(Value::str(interpreter.to_str(&item)?)),
            }
        }
        Ok(())
    }
    let mut flat = Vec::new();
    walk(
        interpreter,
        &args.get(0).cloned().unwrap_or(Value::None),
        &mut flat,
    )?;
    Ok(Value::list(flat))
}

/// A function of the same parameters as `function` that gives `then` of its result, as
/// Snakemake's helpers wrap input functions.
fn wrap(
    function: &Value,
    then: impl Fn(&mut Interpreter, Value) -> Result<Value, Error> + 'static,
) -> Value {
    let (names, _) = parameters(function).unwrap_or_else(|| (vec!["wildcards".into()], false));
    let function = function.clone();
    Value::native(Deferred {
        parameters: names,
        function: Rc::new(move |interpreter, args| {
            let value = interpreter.call(&function, args)?;
            then(interpreter, value)
        }),
    })
}

/// A function of the item of a job that a proxy like `input` names, which gives `then` of it.
fn from_item(
    proxy: &RuleItemProxy,
    then: impl Fn(&mut Interpreter, Value) -> Result<Value, Error> + 'static,
) -> Value {
    let kind = proxy.0;
    deferred(&["wildcards", kind], move |interpreter, mut args| {
        let item = args.take_keyword(kind).unwrap_or(Value::None);
        then(interpreter, item)
    })
}

/// The text of one path given as a text or a `pathlib.Path`.
fn single_path(interpreter: &mut Interpreter, value: &Value) -> Result<String, Error> {
    match value.plain() {
        Value::Str(text) => Ok(text.to_string()),
        Value::Native(_) if value.as_native::<PathObject>().is_some() => interpreter.to_str(value),
        _ => Err(Error::value_error(format!(
            "Values passed to prepend must be a single string or pathlib.Path (or a function returning those). Obtained value: {}",
            interpreter.repr(value)?
        ))),
    }
}

/// `prepend_param(prefix, paths_or_func, space=True)`: each path after the prefix, joined
/// by spaces, like `--input a.txt --input b.txt`.
fn prepend_param(interpreter: &mut Interpreter, mut args: Args) -> Result<Value, Error> {
    let prefix = interpreter.to_str(&args.take(0, "prefix").unwrap_or(Value::None))?;
    let paths = args.take(1, "paths_or_func").unwrap_or(Value::None);
    let space = match args.take(2, "space") {
        Some(value) => interpreter.truthy(&value)?,
        None => true,
    };
    let join = move |interpreter: &mut Interpreter, value: Value| -> Result<Value, Error> {
        let items = match value.plain() {
            Value::Str(_) => vec![value.clone()],
            Value::Native(_) if value.as_native::<PathObject>().is_some() => vec![value.clone()],
            _ => interpreter.iterate(&value)?,
        };
        let mut parts = Vec::with_capacity(items.len());
        for item in &items {
            let path = single_path(interpreter, item)?;
            parts.push(format!("{prefix}{}{path}", if space { " " } else { "" }));
        }
        Ok(Value::str(parts.join(" ")))
    };
    if let Some(proxy) = paths.as_native::<RuleItemProxy>() {
        return Ok(from_item(proxy, join));
    }
    if is_callable(interpreter, &paths) {
        return Ok(wrap(&paths, join));
    }
    join(interpreter, paths)
}

/// The module name of a `.py` file for `python -m`, like `package.module` for
/// `package/module.py`.
fn module_name(path: &str) -> Result<String, Error> {
    let Some(stem) = path.strip_suffix(".py") else {
        return Err(Error::value_error(
            "Only .py files may be run as Python modules.",
        ));
    };
    let components: Vec<&str> = stem.split('/').filter(|part| !part.is_empty()).collect();
    let valid = |component: &str| {
        component
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && component
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
    };
    if components.is_empty() || !components.iter().all(|component| valid(component)) {
        return Err(Error::value_error(format!(
            "{path} does not translate to a valid Python name"
        )));
    }
    Ok(components.join("."))
}

/// `as_py_module(path_or_func=input.script)`.
fn as_py_module(interpreter: &mut Interpreter, mut args: Args) -> Result<Value, Error> {
    let format = |interpreter: &mut Interpreter, value: Value| -> Result<Value, Error> {
        Ok(Value::str(module_name(&path_text(interpreter, &value)?)?))
    };
    let Some(value) = args
        .take(0, "path_or_func")
        .filter(|value| !value.is_none())
    else {
        return Ok(deferred(
            &["wildcards", "input"],
            move |interpreter, mut args| {
                let input = args.take_keyword("input").unwrap_or(Value::None);
                let script = interpreter.get_attribute(&input, "script")?;
                format(interpreter, script)
            },
        ));
    };
    if let Some(proxy) = value.as_native::<RuleItemProxy>() {
        return Ok(from_item(proxy, move |interpreter, item| {
            let files = interpreter.iterate(&item)?;
            match files.as_slice() {
                [file] => format(interpreter, file.clone()),
                _ => Err(Error::value_error(format!(
                    "too many values to unpack (expected 1, got {})",
                    files.len()
                ))),
            }
        }));
    }
    if is_callable(interpreter, &value) {
        return Ok(wrap(&value, format));
    }
    format(interpreter, value)
}

/// The text of a file given by its path or as an open file.
fn file_text(interpreter: &mut Interpreter, file: &Value) -> Result<String, Error> {
    let is_path = matches!(file.plain(), Value::Str(_)) || file.as_native::<PathObject>().is_some();
    if !is_path {
        let read = interpreter.get_attribute(file, "read")?;
        let text = interpreter.call(&read, Args::default())?;
        return interpreter.to_str(&text);
    }
    let path = path_text(interpreter, file)?;
    let resolved = interpreter.host.resolve(&path);
    let bytes = interpreter
        .host
        .read(&resolved)
        .map_err(|error| Error::os_error(&error, &path))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// `extract_checksum(infile, file=...)`: the checksum of a file in a checksum list whose
/// lines hold a checksum, two spaces and a file name, as `md5sum` writes them.
fn extract_checksum(interpreter: &mut Interpreter, mut args: Args) -> Result<Value, Error> {
    let infile = args.take(0, "infile").unwrap_or(Value::None);
    let file = interpreter.to_str(&args.take_keyword("file").unwrap_or(Value::None))?;
    let wanted = file.strip_prefix("./").unwrap_or(&file).to_owned();
    let text = file_text(interpreter, &infile)?;
    let found: Vec<&str> = text
        .lines()
        .filter_map(|line| line.split_once("  "))
        .filter(|(_, name)| name.strip_prefix("./").unwrap_or(name) == wanted)
        .map(|(checksum, _)| checksum)
        .collect();
    match found.as_slice() {
        [checksum] => Ok(Value::str(*checksum)),
        [] => Err(Error::key_error(Value::str(wanted))),
        _ => Err(Error::value_error(
            "can only convert an array of size 1 to a Python scalar",
        )),
    }
}

/// `parse_input(infile, parser=None, **kwargs)`: a function of wildcards and input that reads
/// a file, stripped, or gives the open file to the parser.
fn parse_input(_: &mut Interpreter, mut args: Args) -> Result<Value, Error> {
    let infile = args.take(0, "infile").unwrap_or(Value::None);
    let parser = args.take(1, "parser").unwrap_or(Value::None);
    let keywords = args.keywords;
    Ok(deferred(
        &["wildcards", "input"],
        move |interpreter, mut args| {
            let wildcards = args.positional.first().cloned().unwrap_or(Value::None);
            let input = args.take_keyword("input").unwrap_or(Value::None);
            let file = if is_callable(interpreter, &infile) {
                let mut call = Args::new(vec![wildcards]);
                call.keywords.push(("input".into(), input));
                interpreter.call(&infile, call)?
            } else {
                infile.clone()
            };
            let path = path_text(interpreter, &file)?;
            if parser.is_none() {
                let text = file_text(interpreter, &Value::str(path))?;
                return Ok(Value::str(text.trim()));
            }
            let handle = python_evaluator::builtins::open_file(interpreter, &path, "r", false)?;
            let mut call = Args::new(vec![handle]);
            call.keywords = keywords.clone();
            interpreter.call(&parser, call)
        },
    ))
}

/// Whether a path could be made: its nearest existing ancestor is a directory that the process
/// may read, write and enter.
fn creatable(interpreter: &mut Interpreter, path: &Path) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    if parent.as_os_str().is_empty() || parent == path {
        return false;
    }
    let is_directory = interpreter
        .host
        .stat(parent)
        .is_some_and(|stat| stat.kind == python_evaluator::FileKind::Directory);
    if is_directory && interpreter.host.access(parent, 7) {
        return true;
    }
    creatable(interpreter, parent)
}

/// `choose_file`, `choose_folder` and `choose_tmp`: the first path of a list that exists as
/// the kind asked for with the permissions asked for, or that could be made when `creatable`
/// is true. `choose_tmp` gives `system_tmpdir` when none is.
fn choose(
    interpreter: &mut Interpreter,
    mut args: Args,
    name: &str,
    is_directory: bool,
    creatable_default: Option<bool>,
) -> Result<Value, Error> {
    let paths = args.take(0, "list").unwrap_or(Value::None);
    let mut modes = 0;
    for (index, (keyword, bit, default)) in [
        ("read", 4, true),
        ("write", 2, true),
        (
            if is_directory { "open" } else { "execute" },
            1,
            is_directory,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let asked = match args.take(index + 1, keyword) {
            Some(value) => !value.is_none(),
            None => default,
        };
        if asked {
            modes |= bit;
        }
    }
    let creatable_asked = match args.take(4, "creatable") {
        Some(value) if !value.is_none() => interpreter.truthy(&value)?,
        _ => creatable_default.unwrap_or(false),
    };
    for item in interpreter.iterate(&paths)? {
        let text = python_evaluator::host::expand_home(&path_text(interpreter, &item)?);
        let path = interpreter.host.resolve(&text);
        let kind_matches = interpreter.host.stat(&path).is_some_and(|stat| {
            (stat.kind == python_evaluator::FileKind::Directory) == is_directory
        });
        if kind_matches && interpreter.host.access(&path, modes) {
            return Ok(PathObject::value(&text));
        }
        if creatable_asked && creatable(interpreter, &PathBuf::from(&path)) {
            return Ok(PathObject::value(&text));
        }
    }
    Ok(if name == "choose_tmp" {
        Value::str("system_tmpdir")
    } else {
        Value::None
    })
}
