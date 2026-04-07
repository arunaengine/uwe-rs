//! Native stand-ins for the standard library modules, and for the parts of third-party
//! libraries that workflows call while they are planned.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod collections;
mod data;
mod misc;
mod os;
mod re;

use std::rc::Rc;

use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::value::{Args, Module, Scope, Value};

pub use os::PathObject;
pub use re::{Pattern, compile as compile_regex};

/// The modules that have a native stand-in.
const NATIVE: &[&str] = &[
    "os",
    "os.path",
    "posixpath",
    "sys",
    "re",
    "json",
    "yaml",
    "glob",
    "math",
    "itertools",
    "functools",
    "collections",
    "collections.abc",
    "pathlib",
    "csv",
    "datetime",
    "time",
    "string",
    "textwrap",
    "shutil",
    "subprocess",
    "warnings",
    "logging",
    "socket",
    "platform",
    "getpass",
    "copy",
    "typing",
    "typing_extensions",
    "operator",
    "random",
    "gzip",
    "tempfile",
    "io",
    "dataclasses",
    "abc",
    "contextlib",
    "fnmatch",
    "errno",
    "traceback",
    "pprint",
    "statistics",
    "uuid",
    "pandas",
    "numpy",
    "urllib",
    "urllib.parse",
    "urllib.request",
    "hashlib",
    "inspect",
    "types",
    "enum",
    "builtins",
    "argparse",
    "locale",
    "signal",
    "multiprocessing",
    "threading",
    "concurrent",
    "concurrent.futures",
    "zipfile",
    "tarfile",
    "requests",
    "base64",
    "codecs",
    "struct",
    "decimal",
    "fractions",
    "numbers",
    "keyword",
    "shlex",
    "difflib",
    "bisect",
    "heapq",
    "calendar",
    "zlib",
    "__future__",
    "importlib",
    "pkgutil",
    "atexit",
    "gc",
    "weakref",
    "psutil",
    "distutils",
    "packaging",
    "packaging.version",
    "importlib.metadata",
    "pkg_resources",
    "unicodedata",
];

/// Whether a module name has a native stand-in, so that missing names in it are unsupported
/// rather than errors of the workflow.
pub fn is_native(name: &str) -> bool {
    NATIVE.contains(&name)
}

/// The stand-in module of that name, if there is one.
pub fn native(interpreter: &mut Interpreter, name: &str) -> Result<Option<Value>, Error> {
    let entries: Vec<(&str, Value)> = match name {
        "os" => os::os(interpreter)?,
        "os.path" | "posixpath" => os::path(),
        "glob" => os::glob_module(),
        "shutil" => os::shutil(),
        "pathlib" => os::pathlib(),
        "tempfile" => os::tempfile(),
        "re" => re::module(),
        "json" => data::json(),
        "yaml" => data::yaml(),
        "csv" => data::csv(),
        "gzip" => data::gzip(),
        "itertools" => collections::itertools(),
        "functools" => collections::functools(),
        "collections" => collections::collections(),
        "collections.abc" => collections::abc_types(),
        "operator" => collections::operator(),
        "copy" => collections::copy(),
        "heapq" => collections::heapq(),
        "bisect" => collections::bisect(),
        "sys" => misc::sys(interpreter),
        "math" => misc::math(),
        "datetime" => misc::datetime(),
        "time" => misc::time(),
        "string" => misc::string(),
        "textwrap" => misc::textwrap(),
        "subprocess" => misc::subprocess(),
        "warnings" => misc::warnings(),
        "logging" => misc::logging(),
        "socket" => misc::socket(),
        "platform" => misc::platform(),
        "getpass" => misc::getpass(),
        "typing" | "typing_extensions" => misc::typing(),
        "random" => misc::random(),
        "io" => misc::io(),
        "dataclasses" => misc::dataclasses(),
        "abc" => misc::abc(),
        "contextlib" => misc::contextlib(),
        "fnmatch" => misc::fnmatch(),
        "errno" => misc::errno(),
        "traceback" => misc::traceback(),
        "pprint" => misc::pprint(),
        "statistics" => misc::statistics(),
        "uuid" => misc::uuid(),
        "shlex" => misc::shlex(),
        "keyword" => misc::keyword(),
        "locale" => misc::locale(),
        "__future__" => misc::future(),
        "atexit" => misc::atexit(),
        "gc" => vec![(
            "collect",
            Value::builtin("collect", |_, _| Ok(Value::Int(0))),
        )],
        "signal" | "threading" | "multiprocessing" | "weakref" | "importlib" | "pkgutil"
        | "numbers" => misc::placeholder(name),
        "builtins" => {
            let builtins = interpreter.builtins.clone();
            return Ok(Some(Value::Module(Rc::new(Module {
                name: "builtins".into(),
                globals: builtins,
            }))));
        }
        "pandas" => crate::pandas::module(),
        "urllib" => vec![],
        "concurrent" => vec![],
        "packaging" => vec![],
        "packaging.version" | "distutils" | "pkg_resources" => misc::version_module(),
        "importlib.metadata" => misc::importlib_metadata(),
        "inspect" => misc::inspect(),
        "types" => misc::types_module(),
        "unicodedata" => misc::unicodedata(),
        _ if is_native(name) => {
            // Known library modules without a stand-in yet: importing is fine, using them
            // is unsupported.
            Vec::new()
        }
        _ => return Ok(None),
    };
    Ok(Some(module(name, entries)))
}

/// A module value with the given attributes.
pub fn module(name: &str, entries: Vec<(&str, Value)>) -> Value {
    let globals = Scope::new(None);
    globals.set("__name__", Value::str(name));
    for (key, value) in entries {
        globals.set(key, value);
    }
    Value::Module(Rc::new(Module {
        name: name.into(),
        globals,
    }))
}

/// A builtin function value.
pub fn function(name: &str, function: fn(&mut Interpreter, Args) -> Result<Value, Error>) -> Value {
    Value::builtin(name, function)
}

/// The text of a path argument: a string, or an object with `__fspath__` like `pathlib.Path`.
pub fn path_text(interpreter: &mut Interpreter, value: &Value) -> Result<String, Error> {
    match value.plain() {
        Value::Str(text) => Ok(text.to_string()),
        Value::Bytes(bytes) => Ok(String::from_utf8_lossy(bytes).into_owned()),
        Value::Native(object) if object.type_name() == "PosixPath" || object.instance_of("str") => {
            interpreter.to_str(value)
        }
        Value::Instance(_) => match interpreter.lookup_special(value, "__fspath__") {
            Some(method) => {
                let text = interpreter.call(&method, Args::default())?;
                interpreter.to_str(&text)
            }
            None => Err(Error::type_error(format!(
                "expected str, bytes or os.PathLike object, not {}",
                crate::types::type_name(value)
            ))),
        },
        other => Err(Error::type_error(format!(
            "expected str, bytes or os.PathLike object, not {}",
            crate::types::type_name(other)
        ))),
    }
}
