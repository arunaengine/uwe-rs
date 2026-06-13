//! Native stand-ins for the standard library modules, and for the parts of third-party
//! libraries that workflows call while they are planned.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub mod collections;
pub mod csv;
pub mod data;
mod datetime;
pub(crate) mod enums;
mod files;
pub(crate) mod misc;
mod os;
pub(crate) mod re;
pub mod subprocess;
pub mod system;
mod template;
mod urllib;
mod yte;

use std::rc::Rc;

use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::value::{Args, Module, Scope, Value};

pub use misc::{dedent, logger, shell_quote};
pub use os::{PathObject, make_dirs};
pub use re::{Pattern, compile as compile_regex, translate as translate_regex};

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
    "polars",
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
    "pwd",
    "queue",
    "pickle",
    "fileinput",
    "filecmp",
    "urllib.error",
    "tkinter",
    "configparser",
    "email",
    "email.message",
    "imaplib",
    "smtplib",
    "resource",
    "pyarrow",
    "yte",
    "git",
    "git.exc",
];

/// Whether a module name has a native stand-in, so that missing names in it are unsupported
/// rather than errors of the workflow.
pub fn is_native(name: &str) -> bool {
    NATIVE.contains(&name)
}

/// Names that Python 3 removed from modules that have a stand-in. Code that still uses them
/// gets the `AttributeError` of Python, which it may catch, rather than an unsupported
/// feature.
const REMOVED: &[(&str, &str)] = &[
    ("platform", "linux_distribution"),
    ("platform", "dist"),
    ("time", "clock"),
    ("os", "getcwdu"),
    ("string", "maketrans"),
    ("string", "letters"),
    ("string", "lowercase"),
    ("string", "uppercase"),
    ("sys", "maxint"),
    ("itertools", "izip"),
    ("itertools", "imap"),
    ("itertools", "ifilter"),
    ("collections", "Mapping"),
    ("collections", "MutableMapping"),
    ("collections", "Sequence"),
    ("collections", "Iterable"),
    ("collections", "Callable"),
    ("inspect", "getargspec"),
];

/// The modules of Python's standard library (`sys.stdlib_module_names` of Python 3.13).
const STANDARD_LIBRARY: &[&str] = &[
    "abc",
    "antigravity",
    "argparse",
    "array",
    "ast",
    "asyncio",
    "atexit",
    "base64",
    "bdb",
    "binascii",
    "bisect",
    "builtins",
    "bz2",
    "cProfile",
    "calendar",
    "cmath",
    "cmd",
    "code",
    "codecs",
    "codeop",
    "collections",
    "colorsys",
    "compileall",
    "concurrent",
    "configparser",
    "contextlib",
    "contextvars",
    "copy",
    "copyreg",
    "csv",
    "ctypes",
    "curses",
    "dataclasses",
    "datetime",
    "dbm",
    "decimal",
    "difflib",
    "dis",
    "doctest",
    "email",
    "encodings",
    "ensurepip",
    "enum",
    "errno",
    "faulthandler",
    "fcntl",
    "filecmp",
    "fileinput",
    "fnmatch",
    "fractions",
    "ftplib",
    "functools",
    "gc",
    "genericpath",
    "getopt",
    "getpass",
    "gettext",
    "glob",
    "graphlib",
    "grp",
    "gzip",
    "hashlib",
    "heapq",
    "hmac",
    "html",
    "http",
    "idlelib",
    "imaplib",
    "importlib",
    "inspect",
    "io",
    "ipaddress",
    "itertools",
    "json",
    "keyword",
    "linecache",
    "locale",
    "logging",
    "lzma",
    "mailbox",
    "marshal",
    "math",
    "mimetypes",
    "mmap",
    "modulefinder",
    "msvcrt",
    "multiprocessing",
    "netrc",
    "nt",
    "ntpath",
    "nturl2path",
    "numbers",
    "opcode",
    "operator",
    "optparse",
    "os",
    "pathlib",
    "pdb",
    "pickle",
    "pickletools",
    "pkgutil",
    "platform",
    "plistlib",
    "poplib",
    "posix",
    "posixpath",
    "pprint",
    "profile",
    "pstats",
    "pty",
    "pwd",
    "py_compile",
    "pyclbr",
    "pydoc",
    "pydoc_data",
    "pyexpat",
    "queue",
    "quopri",
    "random",
    "re",
    "readline",
    "reprlib",
    "resource",
    "rlcompleter",
    "runpy",
    "sched",
    "secrets",
    "select",
    "selectors",
    "shelve",
    "shlex",
    "shutil",
    "signal",
    "site",
    "smtplib",
    "socket",
    "socketserver",
    "sqlite3",
    "sre_compile",
    "sre_constants",
    "sre_parse",
    "ssl",
    "stat",
    "statistics",
    "string",
    "stringprep",
    "struct",
    "subprocess",
    "symtable",
    "sys",
    "sysconfig",
    "syslog",
    "tabnanny",
    "tarfile",
    "tempfile",
    "termios",
    "textwrap",
    "this",
    "threading",
    "time",
    "timeit",
    "tkinter",
    "token",
    "tokenize",
    "tomllib",
    "trace",
    "traceback",
    "tracemalloc",
    "tty",
    "turtle",
    "turtledemo",
    "types",
    "typing",
    "unicodedata",
    "unittest",
    "urllib",
    "uuid",
    "venv",
    "warnings",
    "wave",
    "weakref",
    "webbrowser",
    "winreg",
    "winsound",
    "wsgiref",
    "xml",
    "xmlrpc",
    "zipapp",
    "zipfile",
    "zipimport",
    "zlib",
    "zoneinfo",
];

/// Whether a module belongs to Python's standard library, which iunctus may lack a stand-in
/// of, unlike packages that workflows install.
pub fn is_standard_library(name: &str) -> bool {
    let top = name.split('.').next().unwrap_or(name);
    STANDARD_LIBRARY.contains(&top)
}

/// Whether Python 3 removed a name from a module.
pub fn is_removed(module: &str, name: &str) -> bool {
    REMOVED.contains(&(module, name))
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
        "csv" => csv::csv(),
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
        "datetime" => datetime::datetime(),
        "time" => datetime::time(),
        "string" => misc::string(),
        "textwrap" => misc::textwrap(),
        "subprocess" => subprocess::subprocess(),
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
        "resource" => system::resource(),
        "yte" => yte::yte(),
        "enum" => return Ok(Some(enums::module(interpreter)?)),
        "git" | "git.exc" => misc::git(name),
        "signal" | "weakref" | "importlib" | "pkgutil" | "numbers" | "imaplib" | "smtplib"
        | "pyarrow" => misc::placeholder(name),
        "tkinter" => misc::tkinter(),
        "configparser" | "email" | "email.message" => misc::classes_of(name),
        "threading" => system::threading(),
        "multiprocessing" => system::multiprocessing(),
        "pwd" => system::pwd(),
        "psutil" => system::psutil(),
        "queue" => system::queue(),
        "pickle" => system::pickle(),
        "fileinput" => files::fileinput(),
        "filecmp" => files::filecmp(),
        "urllib.parse" => urllib::parse(),
        "urllib.error" => urllib::error(),
        "urllib.request" => urllib::request(),
        "builtins" => {
            let builtins = interpreter.builtins.clone();
            return Ok(Some(Value::Module(Rc::new(Module {
                name: "builtins".into(),
                globals: builtins,
            }))));
        }
        "pandas" => crate::pandas::module(),
        "polars" => crate::pandas::polars::module(),
        "numpy" => crate::pandas::numpy::module(),
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
        Value::Native(_) if let Some(path) = crate::packages::fspath(interpreter, value)? => {
            Ok(path)
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
