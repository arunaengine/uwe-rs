//! `os`, `os.path`, `glob`, `shutil`, `pathlib` and `tempfile`, on the host's files.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::any::Any;
use std::cell::RefCell;

use super::{function, path_text};
use crate::builtins::Record;
use crate::error::Error;
use crate::host::FileKind;
use crate::interpreter::{Interpreter, Operator};
use crate::value::{Args, Dict, Native, Value};

fn text(
    interpreter: &mut Interpreter,
    args: &Args,
    index: usize,
    name: &str,
) -> Result<String, Error> {
    match args.positional.get(index) {
        Some(value) => path_text(interpreter, value),
        None => Err(Error::type_error(format!(
            "{name}() missing required argument"
        ))),
    }
}

pub fn os(interpreter: &mut Interpreter) -> Result<Vec<(&'static str, Value)>, Error> {
    let environment = Value::dict_of(
        interpreter
            .host
            .environment()
            .into_iter()
            .map(|(key, value)| (key, Value::str(value))),
    );
    let path_module = interpreter.import_module("os.path")?;
    Ok(vec![
        ("path", path_module),
        ("environ", environment),
        ("sep", Value::str("/")),
        ("altsep", Value::None),
        ("pathsep", Value::str(":")),
        ("linesep", Value::str("\n")),
        ("curdir", Value::str(".")),
        ("pardir", Value::str("..")),
        ("extsep", Value::str(".")),
        ("devnull", Value::str("/dev/null")),
        ("name", Value::str("posix")),
        ("F_OK", Value::Int(0)),
        ("R_OK", Value::Int(4)),
        ("W_OK", Value::Int(2)),
        ("X_OK", Value::Int(1)),
        (
            "PathLike",
            Value::Class(crate::types::native_class("PathLike")),
        ),
        (
            "getenv",
            function("getenv", |interpreter, args| {
                args.expect("getenv", 1, 2)?;
                let name = args.positional[0].as_str().unwrap_or_default().to_owned();
                let environ = interpreter.import_module("os")?;
                let environ = interpreter.get_attribute(&environ, "environ")?;
                let found = interpreter.get_item(&environ, &Value::str(name));
                Ok(
                    found
                        .unwrap_or_else(|_| args.positional.get(1).cloned().unwrap_or(Value::None)),
                )
            }),
        ),
        ("putenv", function("putenv", |_, _| Ok(Value::None))),
        (
            "getcwd",
            function("getcwd", |interpreter, _| {
                Ok(Value::str(
                    interpreter.host.cwd().to_string_lossy().as_ref(),
                ))
            }),
        ),
        (
            "chdir",
            function("chdir", |interpreter, args| {
                let path = text(interpreter, &args, 0, "chdir")?;
                let resolved = interpreter.host.resolve(&path);
                if interpreter
                    .host
                    .stat(&resolved)
                    .is_none_or(|stat| stat.kind != FileKind::Directory)
                {
                    return Err(Error::file_not_found(&path));
                }
                interpreter.host.set_cwd(&resolved);
                Ok(Value::None)
            }),
        ),
        (
            "listdir",
            function("listdir", |interpreter, args| {
                let path = match args.positional.first() {
                    Some(path) => path_text(interpreter, path)?,
                    None => ".".into(),
                };
                let resolved = interpreter.host.resolve(&path);
                let names = interpreter
                    .host
                    .list(&resolved)
                    .map_err(|error| Error::os_error(&error, &path))?;
                Ok(Value::list(names.into_iter().map(Value::str).collect()))
            }),
        ),
        (
            "scandir",
            function("scandir", |interpreter, args| {
                let path = match args.positional.first() {
                    Some(path) => path_text(interpreter, path)?,
                    None => ".".into(),
                };
                let resolved = interpreter.host.resolve(&path);
                let names = interpreter
                    .host
                    .list(&resolved)
                    .map_err(|error| Error::os_error(&error, &path))?;
                let mut entries = Vec::new();
                for name in names {
                    let kind = interpreter
                        .host
                        .stat(&resolved.join(&name))
                        .map(|stat| stat.kind);
                    let mut record = Dict::default();
                    record.insert("name", Value::str(name.as_str()));
                    record.insert("path", Value::str(join_two(&path, &name)));
                    let is_dir = kind == Some(FileKind::Directory);
                    let is_file = kind == Some(FileKind::File);
                    record.insert(
                        "is_dir",
                        Value::builtin("is_dir", move |_, _| Ok(Value::Bool(is_dir))),
                    );
                    record.insert(
                        "is_file",
                        Value::builtin("is_file", move |_, _| Ok(Value::Bool(is_file))),
                    );
                    record.insert(
                        "is_symlink",
                        Value::builtin("is_symlink", |_, _| Ok(Value::Bool(false))),
                    );
                    entries.push(Value::native(Record(RefCell::new(record), "DirEntry")));
                }
                Ok(Value::native(ContextList(Value::list(entries))))
            }),
        ),
        ("walk", function("walk", walk)),
        (
            "makedirs",
            function("makedirs", |interpreter, mut args| {
                let exist_ok = args
                    .take_keyword("exist_ok")
                    .is_some_and(|value| matches!(value.plain(), Value::Bool(true)));
                let path = text(interpreter, &args, 0, "makedirs")?;
                let resolved = interpreter.host.resolve(&path);
                if interpreter.host.stat(&resolved).is_some() && !exist_ok {
                    return Err(Error::os_error(
                        &std::io::ErrorKind::AlreadyExists.into(),
                        &path,
                    ));
                }
                make_dirs(interpreter, &resolved)?;
                Ok(Value::None)
            }),
        ),
        (
            "mkdir",
            function("mkdir", |interpreter, args| {
                let path = text(interpreter, &args, 0, "mkdir")?;
                let resolved = interpreter.host.resolve(&path);
                if interpreter.host.stat(&resolved).is_some() {
                    return Err(Error::os_error(
                        &std::io::ErrorKind::AlreadyExists.into(),
                        &path,
                    ));
                }
                interpreter
                    .host
                    .make_dir(&resolved)
                    .map_err(|error| Error::os_error(&error, &path))?;
                Ok(Value::None)
            }),
        ),
        ("remove", function("remove", remove)),
        ("unlink", function("unlink", remove)),
        ("rmdir", function("rmdir", remove)),
        ("removedirs", function("removedirs", remove)),
        ("rename", function("rename", rename)),
        ("replace", function("replace", rename)),
        ("symlink", function("symlink", |_, _| Ok(Value::None))),
        ("link", function("link", |_, _| Ok(Value::None))),
        ("chmod", function("chmod", |_, _| Ok(Value::None))),
        ("utime", function("utime", |_, _| Ok(Value::None))),
        ("umask", function("umask", |_, _| Ok(Value::Int(0o22)))),
        (
            "access",
            function("access", |interpreter, args| {
                let path = text(interpreter, &args, 0, "access")?;
                let resolved = interpreter.host.resolve(&path);
                Ok(Value::Bool(interpreter.host.stat(&resolved).is_some()))
            }),
        ),
        ("stat", function("stat", stat)),
        ("lstat", function("lstat", stat)),
        (
            "fspath",
            function("fspath", |interpreter, args| {
                args.expect("fspath", 1, 1)?;
                Ok(Value::str(path_text(interpreter, &args.positional[0])?))
            }),
        ),
        (
            "cpu_count",
            function("cpu_count", |_, _| {
                Ok(Value::Int(
                    std::thread::available_parallelism().map_or(1, |count| count.get()) as i64,
                ))
            }),
        ),
        ("getpid", function("getpid", |_, _| Ok(Value::Int(12345)))),
        ("getppid", function("getppid", |_, _| Ok(Value::Int(1)))),
        ("getuid", function("getuid", |_, _| Ok(Value::Int(1000)))),
        ("getgid", function("getgid", |_, _| Ok(Value::Int(1000)))),
        (
            "getlogin",
            function("getlogin", |_, _| Ok(Value::str(user_name()))),
        ),
        (
            "uname",
            function("uname", |_, _| {
                let mut record = Dict::default();
                record.insert("sysname", Value::str("Linux"));
                record.insert("nodename", Value::str("localhost"));
                record.insert("release", Value::str("6.0.0"));
                record.insert("version", Value::str("#1"));
                record.insert("machine", Value::str("x86_64"));
                Ok(Value::native(Record(RefCell::new(record), "uname_result")))
            }),
        ),
        (
            "system",
            function("system", |_, _| {
                Err(Error::unsupported(
                    "os.system: running commands while the workflow is planned",
                ))
            }),
        ),
        (
            "popen",
            function("popen", |_, _| {
                Err(Error::unsupported(
                    "os.popen: running commands while the workflow is planned",
                ))
            }),
        ),
        (
            "sched_getaffinity",
            function("sched_getaffinity", |_, _| {
                let count = std::thread::available_parallelism().map_or(1, |count| count.get());
                let mut members = indexmap::IndexSet::new();
                for index in 0..count {
                    members.insert(crate::value::Key(Value::Int(index as i64)));
                }
                Ok(Value::set(members))
            }),
        ),
    ])
}

fn user_name() -> String {
    std::env::var("USER").unwrap_or_else(|_| "user".into())
}

/// A list that also works as a context manager, like the iterator of `os.scandir`.
struct ContextList(Value);

impl Native for ContextList {
    fn type_name(&self) -> &str {
        "ScandirIterator"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn iterate(&self, interpreter: &mut Interpreter) -> Result<Option<Vec<Value>>, Error> {
        interpreter.iterate(&self.0).map(Some)
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        this: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        let this = this.clone();
        Ok(match name {
            "__enter__" => Some(Value::builtin("__enter__", move |_, _| Ok(this.clone()))),
            "__exit__" | "close" => Some(Value::builtin("__exit__", |_, _| Ok(Value::Bool(false)))),
            _ => None,
        })
    }
}

fn make_dirs(interpreter: &mut Interpreter, path: &std::path::Path) -> Result<(), Error> {
    let mut missing = Vec::new();
    for ancestor in path.ancestors() {
        if interpreter.host.stat(ancestor).is_some() {
            break;
        }
        missing.push(ancestor.to_owned());
    }
    for directory in missing.into_iter().rev() {
        interpreter
            .host
            .make_dir(&directory)
            .map_err(|error| Error::os_error(&error, &directory.to_string_lossy()))?;
    }
    Ok(())
}

fn remove(interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    let path = text(interpreter, &args, 0, "remove")?;
    let resolved = interpreter.host.resolve(&path);
    if interpreter.host.stat(&resolved).is_none() {
        return Err(Error::file_not_found(&path));
    }
    interpreter
        .host
        .remove(&resolved)
        .map_err(|error| Error::os_error(&error, &path))?;
    Ok(Value::None)
}

fn rename(interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    let from = text(interpreter, &args, 0, "rename")?;
    let to = text(interpreter, &args, 1, "rename")?;
    let (from_path, to_path) = (
        interpreter.host.resolve(&from),
        interpreter.host.resolve(&to),
    );
    let data = interpreter
        .host
        .read(&from_path)
        .map_err(|error| Error::os_error(&error, &from))?;
    interpreter
        .host
        .write(&to_path, &data, false)
        .map_err(|error| Error::os_error(&error, &to))?;
    interpreter
        .host
        .remove(&from_path)
        .map_err(|error| Error::os_error(&error, &from))?;
    Ok(Value::None)
}

fn stat(interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    let path = text(interpreter, &args, 0, "stat")?;
    let resolved = interpreter.host.resolve(&path);
    let stat = interpreter
        .host
        .stat(&resolved)
        .ok_or_else(|| Error::file_not_found(&path))?;
    let mut record = Dict::default();
    record.insert("st_size", Value::Int(stat.size as i64));
    record.insert("st_mtime", Value::Float(stat.modified));
    record.insert("st_ctime", Value::Float(stat.modified));
    record.insert("st_atime", Value::Float(stat.modified));
    record.insert(
        "st_mode",
        Value::Int(if stat.kind == FileKind::Directory {
            0o40755
        } else {
            0o100644
        }),
    );
    Ok(Value::native(Record(RefCell::new(record), "stat_result")))
}

fn walk(interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    let top = text(interpreter, &args, 0, "walk")?;
    let mut results = Vec::new();
    let mut pending = vec![top];
    while let Some(directory) = pending.pop() {
        let resolved = interpreter.host.resolve(&directory);
        let Ok(names) = interpreter.host.list(&resolved) else {
            continue;
        };
        let mut directories = Vec::new();
        let mut files = Vec::new();
        for name in names {
            match interpreter
                .host
                .stat(&resolved.join(&name))
                .map(|stat| stat.kind)
            {
                Some(FileKind::Directory) => directories.push(name),
                _ => files.push(name),
            }
        }
        for name in directories.iter().rev() {
            pending.push(join_two(&directory, name));
        }
        results.push(Value::tuple(vec![
            Value::str(directory.as_str()),
            Value::list(directories.into_iter().map(Value::str).collect()),
            Value::list(files.into_iter().map(Value::str).collect()),
        ]));
    }
    Ok(Value::iterator(results))
}

/// `os.path.join` of two parts.
pub fn join_two(a: &str, b: &str) -> String {
    if b.starts_with('/') || a.is_empty() {
        b.to_owned()
    } else if a.ends_with('/') {
        format!("{a}{b}")
    } else {
        format!("{a}/{b}")
    }
}

/// `os.path.split`.
pub fn split(path: &str) -> (String, String) {
    let index = path.rfind('/').map_or(0, |index| index + 1);
    let (head, tail) = path.split_at(index);
    let trimmed = head.trim_end_matches('/');
    let head = if trimmed.is_empty() { head } else { trimmed };
    (head.to_owned(), tail.to_owned())
}

/// `os.path.splitext`.
pub fn splitext(path: &str) -> (String, String) {
    let base_start = path.rfind('/').map_or(0, |index| index + 1);
    let base = &path[base_start..];
    let leading = base.len() - base.trim_start_matches('.').len();
    match base[leading..].rfind('.') {
        Some(dot) => {
            let index = base_start + leading + dot;
            (path[..index].to_owned(), path[index..].to_owned())
        }
        None => (path.to_owned(), String::new()),
    }
}

/// `os.path.normpath` for POSIX paths.
pub fn normpath(path: &str) -> String {
    if path.is_empty() {
        return ".".into();
    }
    let initial = if path.starts_with("//") && !path.starts_with("///") {
        2
    } else if path.starts_with('/') {
        1
    } else {
        0
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if !parts.is_empty() && parts.last() != Some(&"..") {
                    parts.pop();
                } else if initial == 0 {
                    parts.push("..");
                }
            }
            other => parts.push(other),
        }
    }
    let body = parts.join("/");
    let normal = format!("{}{body}", "/".repeat(initial));
    if normal.is_empty() {
        ".".into()
    } else {
        normal
    }
}

pub fn abspath(interpreter: &Interpreter, path: &str) -> String {
    let path = crate::host::expand_home(path);
    if path.starts_with('/') {
        normpath(&path)
    } else {
        normpath(&join_two(&interpreter.host.cwd().to_string_lossy(), &path))
    }
}

fn relpath(path: &str, start: &str) -> String {
    let path_parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    let start_parts: Vec<&str> = start.split('/').filter(|part| !part.is_empty()).collect();
    let common = path_parts
        .iter()
        .zip(&start_parts)
        .take_while(|(a, b)| a == b)
        .count();
    let mut parts: Vec<&str> = vec![".."; start_parts.len() - common];
    parts.extend(&path_parts[common..]);
    if parts.is_empty() {
        ".".into()
    } else {
        parts.join("/")
    }
}

fn expandvars(interpreter: &mut Interpreter, path: &str) -> String {
    let environment: std::collections::HashMap<String, String> =
        interpreter.host.environment().into_iter().collect();
    let mut result = String::new();
    let chars: Vec<char> = path.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '$' {
            let (name, end) = if chars.get(index + 1) == Some(&'{') {
                match chars[index + 2..].iter().position(|c| *c == '}') {
                    Some(close) => (
                        chars[index + 2..index + 2 + close]
                            .iter()
                            .collect::<String>(),
                        index + 3 + close,
                    ),
                    None => (String::new(), index),
                }
            } else {
                let length = chars[index + 1..]
                    .iter()
                    .take_while(|c| c.is_alphanumeric() || **c == '_')
                    .count();
                (
                    chars[index + 1..index + 1 + length].iter().collect(),
                    index + 1 + length,
                )
            };
            if !name.is_empty()
                && let Some(value) = environment.get(&name)
            {
                result.push_str(value);
                index = end;
                continue;
            }
        }
        result.push(chars[index]);
        index += 1;
    }
    result
}

fn kind_of(
    interpreter: &mut Interpreter,
    args: &Args,
    name: &str,
) -> Result<Option<FileKind>, Error> {
    let Some(value) = args.positional.first() else {
        return Err(Error::type_error(format!(
            "{name}() missing required argument"
        )));
    };
    if value.is_none() {
        return Ok(None);
    }
    let path = path_text(interpreter, value)?;
    if path.is_empty() {
        return Ok(None);
    }
    let resolved = interpreter.host.resolve(&path);
    Ok(interpreter.host.stat(&resolved).map(|stat| stat.kind))
}

pub fn path() -> Vec<(&'static str, Value)> {
    vec![
        ("sep", Value::str("/")),
        ("curdir", Value::str(".")),
        ("pardir", Value::str("..")),
        ("extsep", Value::str(".")),
        (
            "join",
            function("join", |interpreter, args| {
                args.expect("join", 1, usize::MAX)?;
                let mut result = path_text(interpreter, &args.positional[0])?;
                for part in &args.positional[1..] {
                    let part = path_text(interpreter, part)?;
                    result = join_two(&result, &part);
                }
                Ok(Value::str(result))
            }),
        ),
        (
            "exists",
            function("exists", |interpreter, args| {
                Ok(Value::Bool(
                    kind_of(interpreter, &args, "exists")?.is_some(),
                ))
            }),
        ),
        (
            "lexists",
            function("lexists", |interpreter, args| {
                Ok(Value::Bool(
                    kind_of(interpreter, &args, "lexists")?.is_some(),
                ))
            }),
        ),
        (
            "isfile",
            function("isfile", |interpreter, args| {
                Ok(Value::Bool(
                    kind_of(interpreter, &args, "isfile")? == Some(FileKind::File),
                ))
            }),
        ),
        (
            "isdir",
            function("isdir", |interpreter, args| {
                Ok(Value::Bool(
                    kind_of(interpreter, &args, "isdir")? == Some(FileKind::Directory),
                ))
            }),
        ),
        ("islink", function("islink", |_, _| Ok(Value::Bool(false)))),
        (
            "ismount",
            function("ismount", |interpreter, args| {
                let path = text(interpreter, &args, 0, "ismount")?;
                Ok(Value::Bool(path == "/"))
            }),
        ),
        (
            "isabs",
            function("isabs", |interpreter, args| {
                let path = text(interpreter, &args, 0, "isabs")?;
                Ok(Value::Bool(path.starts_with('/')))
            }),
        ),
        (
            "basename",
            function("basename", |interpreter, args| {
                let path = text(interpreter, &args, 0, "basename")?;
                Ok(Value::str(split(&path).1))
            }),
        ),
        (
            "dirname",
            function("dirname", |interpreter, args| {
                let path = text(interpreter, &args, 0, "dirname")?;
                Ok(Value::str(split(&path).0))
            }),
        ),
        (
            "split",
            function("split", |interpreter, args| {
                let path = text(interpreter, &args, 0, "split")?;
                let (head, tail) = split(&path);
                Ok(Value::tuple(vec![Value::str(head), Value::str(tail)]))
            }),
        ),
        (
            "splitext",
            function("splitext", |interpreter, args| {
                let path = text(interpreter, &args, 0, "splitext")?;
                let (root, extension) = splitext(&path);
                Ok(Value::tuple(vec![Value::str(root), Value::str(extension)]))
            }),
        ),
        (
            "splitdrive",
            function("splitdrive", |interpreter, args| {
                let path = text(interpreter, &args, 0, "splitdrive")?;
                Ok(Value::tuple(vec![Value::str(""), Value::str(path)]))
            }),
        ),
        (
            "normpath",
            function("normpath", |interpreter, args| {
                let path = text(interpreter, &args, 0, "normpath")?;
                Ok(Value::str(normpath(&path)))
            }),
        ),
        (
            "abspath",
            function("abspath", |interpreter, args| {
                let path = text(interpreter, &args, 0, "abspath")?;
                Ok(Value::str(abspath(interpreter, &path)))
            }),
        ),
        (
            "realpath",
            function("realpath", |interpreter, mut args| {
                args.take_keyword("strict");
                let path = text(interpreter, &args, 0, "realpath")?;
                Ok(Value::str(abspath(interpreter, &path)))
            }),
        ),
        (
            "relpath",
            function("relpath", |interpreter, args| {
                let path = text(interpreter, &args, 0, "relpath")?;
                let start = match args.positional.get(1) {
                    Some(start) if !start.is_none() => path_text(interpreter, start)?,
                    _ => ".".into(),
                };
                let path = abspath(interpreter, &path);
                let start = abspath(interpreter, &start);
                Ok(Value::str(relpath(&path, &start)))
            }),
        ),
        (
            "expanduser",
            function("expanduser", |interpreter, args| {
                let path = text(interpreter, &args, 0, "expanduser")?;
                Ok(Value::str(crate::host::expand_home(&path)))
            }),
        ),
        (
            "expandvars",
            function("expandvars", |interpreter, args| {
                let path = text(interpreter, &args, 0, "expandvars")?;
                Ok(Value::str(expandvars(interpreter, &path)))
            }),
        ),
        (
            "getsize",
            function("getsize", |interpreter, args| {
                let path = text(interpreter, &args, 0, "getsize")?;
                let resolved = interpreter.host.resolve(&path);
                let stat = interpreter
                    .host
                    .stat(&resolved)
                    .ok_or_else(|| Error::file_not_found(&path))?;
                Ok(Value::Int(stat.size as i64))
            }),
        ),
        (
            "getmtime",
            function("getmtime", |interpreter, args| {
                let path = text(interpreter, &args, 0, "getmtime")?;
                let resolved = interpreter.host.resolve(&path);
                let stat = interpreter
                    .host
                    .stat(&resolved)
                    .ok_or_else(|| Error::file_not_found(&path))?;
                Ok(Value::Float(stat.modified))
            }),
        ),
        (
            "getctime",
            function("getctime", |interpreter, args| {
                let path = text(interpreter, &args, 0, "getctime")?;
                let resolved = interpreter.host.resolve(&path);
                let stat = interpreter
                    .host
                    .stat(&resolved)
                    .ok_or_else(|| Error::file_not_found(&path))?;
                Ok(Value::Float(stat.modified))
            }),
        ),
        (
            "commonprefix",
            function("commonprefix", |interpreter, args| {
                args.expect("commonprefix", 1, 1)?;
                let items = interpreter.iterate(&args.positional[0])?;
                let texts: Vec<String> = items
                    .iter()
                    .map(|item| path_text(interpreter, item))
                    .collect::<Result<_, _>>()?;
                let Some(first) = texts.first() else {
                    return Ok(Value::str(""));
                };
                let mut prefix: Vec<char> = first.chars().collect();
                for other in &texts[1..] {
                    let common = prefix
                        .iter()
                        .zip(other.chars())
                        .take_while(|(a, b)| **a == *b)
                        .count();
                    prefix.truncate(common);
                }
                Ok(Value::str(prefix.into_iter().collect::<String>()))
            }),
        ),
        (
            "commonpath",
            function("commonpath", |interpreter, args| {
                args.expect("commonpath", 1, 1)?;
                let items = interpreter.iterate(&args.positional[0])?;
                let texts: Vec<String> = items
                    .iter()
                    .map(|item| path_text(interpreter, item))
                    .collect::<Result<_, _>>()?;
                let Some(first) = texts.first() else {
                    return Err(Error::value_error("commonpath() arg is an empty sequence"));
                };
                let absolute = first.starts_with('/');
                let mut common: Vec<&str> = first
                    .split('/')
                    .filter(|part| !part.is_empty() && *part != ".")
                    .collect();
                for other in &texts[1..] {
                    let parts: Vec<&str> = other
                        .split('/')
                        .filter(|part| !part.is_empty() && *part != ".")
                        .collect();
                    let shared = common
                        .iter()
                        .zip(&parts)
                        .take_while(|(a, b)| a == b)
                        .count();
                    common.truncate(shared);
                }
                let joined = common.join("/");
                Ok(Value::str(if absolute {
                    format!("/{joined}")
                } else {
                    joined
                }))
            }),
        ),
        (
            "samefile",
            function("samefile", |interpreter, args| {
                let a = text(interpreter, &args, 0, "samefile")?;
                let b = text(interpreter, &args, 1, "samefile")?;
                Ok(Value::Bool(
                    abspath(interpreter, &a) == abspath(interpreter, &b),
                ))
            }),
        ),
    ]
}

/// Converts a shell glob part to a regular expression, as `fnmatch.translate` does.
pub fn glob_regex(pattern: &str) -> String {
    let mut regex = String::from("(?s:");
    let chars: Vec<char> = pattern.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        index += 1;
        match c {
            '*' => regex.push_str(".*"),
            '?' => regex.push('.'),
            '[' => {
                let mut end = index;
                if end < chars.len() && chars[end] == '!' {
                    end += 1;
                }
                if end < chars.len() && chars[end] == ']' {
                    end += 1;
                }
                while end < chars.len() && chars[end] != ']' {
                    end += 1;
                }
                if end >= chars.len() {
                    regex.push_str("\\[");
                } else {
                    let mut class: String = chars[index..end].iter().collect();
                    index = end + 1;
                    class = class.replace('\\', "\\\\");
                    if let Some(rest) = class.strip_prefix('!') {
                        class = format!("^{rest}");
                    }
                    regex.push('[');
                    regex.push_str(&class);
                    regex.push(']');
                }
            }
            other => regex.push_str(&fancy_regex::escape(&other.to_string())),
        }
    }
    regex.push_str(")\\z");
    regex
}

fn has_magic(text: &str) -> bool {
    text.contains(['*', '?', '['])
}
