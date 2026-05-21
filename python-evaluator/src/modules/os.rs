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
                let mode = args.get(1).and_then(Value::as_int).unwrap_or(0);
                let resolved = interpreter.host.resolve(&path);
                Ok(Value::Bool(interpreter.host.access(&resolved, mode)))
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
            "strerror",
            function("strerror", |_, args| {
                let code = args.get(0).and_then(Value::as_int).unwrap_or(0);
                let error = std::io::Error::from_raw_os_error(code as i32);
                let text = error.to_string();
                // The text without Rust's " (os error N)".
                let text = text
                    .split(" (os error")
                    .next()
                    .unwrap_or_default()
                    .to_owned();
                Ok(Value::str(text))
            }),
        ),
        ("system", function("system", super::subprocess::system)),
        ("popen", function("popen", super::subprocess::os_popen)),
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

/// Makes a folder and the folders above it that are missing, as `os.makedirs` does.
pub fn make_dirs(interpreter: &mut Interpreter, path: &std::path::Path) -> Result<(), Error> {
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

/// `glob.glob`: paths that match a pattern, spelled as the pattern is.
pub fn glob(
    interpreter: &mut Interpreter,
    pattern: &str,
    recursive: bool,
) -> Result<Vec<String>, Error> {
    let absolute = pattern.starts_with('/');
    let parts: Vec<&str> = pattern.split('/').filter(|part| !part.is_empty()).collect();
    let mut results: Vec<String> = vec![if absolute { "/".into() } else { String::new() }];
    for (position, part) in parts.iter().enumerate() {
        let last = position + 1 == parts.len();
        let mut next = Vec::new();
        for base in &results {
            let base_resolved = interpreter
                .host
                .resolve(if base.is_empty() { "." } else { base });
            if recursive && *part == "**" {
                // Zero or more directories.
                let mut stack = vec![base.clone()];
                while let Some(directory) = stack.pop() {
                    next.push(directory.clone());
                    let resolved = interpreter.host.resolve(if directory.is_empty() {
                        "."
                    } else {
                        &directory
                    });
                    let Ok(names) = interpreter.host.list(&resolved) else {
                        continue;
                    };
                    for name in names.iter().rev() {
                        if name.starts_with('.') {
                            continue;
                        }
                        if interpreter
                            .host
                            .stat(&resolved.join(name))
                            .is_some_and(|stat| stat.kind == FileKind::Directory)
                        {
                            stack.push(join_prefix(&directory, name));
                        } else if last {
                            next.push(join_prefix(&directory, name));
                        }
                    }
                }
                continue;
            }
            if !has_magic(part) {
                let candidate = join_prefix(base, part);
                let resolved = interpreter.host.resolve(&candidate);
                let exists = interpreter.host.stat(&resolved);
                if exists.is_some_and(|stat| last || stat.kind == FileKind::Directory) {
                    next.push(candidate);
                }
                continue;
            }
            let Ok(names) = interpreter.host.list(&base_resolved) else {
                continue;
            };
            let regex = fancy_regex::Regex::new(&glob_regex(part))
                .map_err(|error| Error::value_error(error.to_string()))?;
            for name in names {
                if name.starts_with('.') && !part.starts_with('.') {
                    continue;
                }
                if !regex.is_match(&name).unwrap_or(false) {
                    continue;
                }
                let candidate = join_prefix(base, &name);
                if !last {
                    let resolved = interpreter.host.resolve(&candidate);
                    if interpreter
                        .host
                        .stat(&resolved)
                        .is_none_or(|stat| stat.kind != FileKind::Directory)
                    {
                        continue;
                    }
                }
                next.push(candidate);
            }
        }
        results = next;
    }
    if parts.is_empty() {
        return Ok(Vec::new());
    }
    if pattern.ends_with('/') {
        results = results
            .into_iter()
            .map(|result| format!("{result}/"))
            .collect();
    }
    results.dedup();
    Ok(results)
}

fn join_prefix(base: &str, name: &str) -> String {
    if base.is_empty() {
        name.to_owned()
    } else {
        join_two(base, name)
    }
}

pub fn glob_module() -> Vec<(&'static str, Value)> {
    fn run(interpreter: &mut Interpreter, mut args: Args) -> Result<Vec<Value>, Error> {
        let recursive = args
            .take_keyword("recursive")
            .is_some_and(|value| matches!(value.plain(), Value::Bool(true)));
        args.take_keyword("root_dir");
        let pattern = text(interpreter, &args, 0, "glob")?;
        Ok(glob(interpreter, &pattern, recursive)?
            .into_iter()
            .map(Value::str)
            .collect())
    }
    vec![
        (
            "glob",
            function("glob", |interpreter, args| {
                Ok(Value::list(run(interpreter, args)?))
            }),
        ),
        (
            "iglob",
            function("iglob", |interpreter, args| {
                Ok(Value::iterator(run(interpreter, args)?))
            }),
        ),
        (
            "escape",
            function("escape", |interpreter, args| {
                let path = text(interpreter, &args, 0, "escape")?;
                let mut escaped = String::new();
                for c in path.chars() {
                    if matches!(c, '*' | '?' | '[') {
                        escaped.push('[');
                        escaped.push(c);
                        escaped.push(']');
                    } else {
                        escaped.push(c);
                    }
                }
                Ok(Value::str(escaped))
            }),
        ),
        (
            "has_magic",
            function("has_magic", |interpreter, args| {
                let path = text(interpreter, &args, 0, "has_magic")?;
                Ok(Value::Bool(has_magic(&path)))
            }),
        ),
    ]
}

pub fn shutil() -> Vec<(&'static str, Value)> {
    fn copy(interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
        let from = text(interpreter, &args, 0, "copy")?;
        let mut to = text(interpreter, &args, 1, "copy")?;
        let from_path = interpreter.host.resolve(&from);
        let mut to_path = interpreter.host.resolve(&to);
        if interpreter
            .host
            .stat(&to_path)
            .is_some_and(|stat| stat.kind == FileKind::Directory)
        {
            let name = split(&from).1;
            to = join_two(&to, &name);
            to_path = to_path.join(name);
        }
        let data = interpreter
            .host
            .read(&from_path)
            .map_err(|error| Error::os_error(&error, &from))?;
        interpreter
            .host
            .write(&to_path, &data, false)
            .map_err(|error| Error::os_error(&error, &to))?;
        Ok(Value::str(to))
    }
    vec![
        ("which", function("which", |_, _| Ok(Value::None))),
        ("copy", function("copy", copy)),
        ("copy2", function("copy2", copy)),
        ("copyfile", function("copyfile", copy)),
        (
            "move",
            function("move", |interpreter, args| {
                let result = copy(interpreter, args.clone())?;
                remove(interpreter, Args::new(vec![args.positional[0].clone()]))?;
                Ok(result)
            }),
        ),
        (
            "rmtree",
            function("rmtree", |interpreter, mut args| {
                let ignore = args
                    .take(1, "ignore_errors")
                    .is_some_and(|value| matches!(value.plain(), Value::Bool(true)));
                match remove(interpreter, args) {
                    Ok(value) => Ok(value),
                    Err(_) if ignore => Ok(Value::None),
                    Err(error) => Err(error),
                }
            }),
        ),
        (
            "copytree",
            function("copytree", |_, _| {
                Err(Error::unsupported(
                    "shutil.copytree while the workflow is planned",
                ))
            }),
        ),
        (
            "disk_usage",
            function("disk_usage", |_, _| {
                Err(Error::unsupported("shutil.disk_usage"))
            }),
        ),
    ]
}

pub fn tempfile() -> Vec<(&'static str, Value)> {
    vec![
        (
            "gettempdir",
            function("gettempdir", |_, _| Ok(Value::str("/tmp"))),
        ),
        ("tempdir", Value::None),
        (
            "mkdtemp",
            function("mkdtemp", |interpreter, _| {
                let path = "/tmp/iunctus-temporary";
                interpreter
                    .host
                    .make_dir(std::path::Path::new(path))
                    .map_err(|error| Error::os_error(&error, path))?;
                Ok(Value::str(path))
            }),
        ),
        (
            "mkstemp",
            function("mkstemp", |_, _| {
                Err(Error::unsupported("tempfile.mkstemp"))
            }),
        ),
        (
            "NamedTemporaryFile",
            function("NamedTemporaryFile", |_, _| {
                Err(Error::unsupported("tempfile.NamedTemporaryFile"))
            }),
        ),
        (
            "TemporaryDirectory",
            function("TemporaryDirectory", |_, _| {
                Err(Error::unsupported("tempfile.TemporaryDirectory"))
            }),
        ),
    ]
}

/// `pathlib.Path`: a path as text in pathlib's normal form.
#[derive(Clone, Debug)]
pub struct PathObject {
    pub path: String,
}

impl PathObject {
    /// Normalizes like pathlib: no empty or `.` parts, no trailing slash, `.` for nothing.
    pub fn new(text: &str) -> PathObject {
        let absolute = text.starts_with('/');
        let parts: Vec<&str> = text
            .split('/')
            .filter(|part| !part.is_empty() && *part != ".")
            .collect();
        let body = parts.join("/");
        let path = match (absolute, body.is_empty()) {
            (true, _) => format!("/{body}"),
            (false, true) => ".".into(),
            (false, false) => body,
        };
        PathObject { path }
    }

    pub fn value(text: &str) -> Value {
        Value::native(PathObject::new(text))
    }

    fn join(&self, other: &str) -> PathObject {
        if other.starts_with('/') || self.path == "." {
            PathObject::new(other)
        } else {
            PathObject::new(&format!("{}/{other}", self.path))
        }
    }

    fn name(&self) -> String {
        if self.path == "." || self.path == "/" {
            return String::new();
        }
        split(&self.path).1
    }

    fn parent(&self) -> PathObject {
        if self.path == "/" || self.path == "." {
            return self.clone();
        }
        let (head, _) = split(&self.path);
        PathObject::new(if head.is_empty() { "." } else { &head })
    }

    fn suffix(&self) -> String {
        let name = self.name();
        let (_, extension) = splitext(&name);
        if extension == "." {
            String::new()
        } else {
            extension
        }
    }

    fn stem(&self) -> String {
        let name = self.name();
        let suffix = self.suffix();
        name[..name.len() - suffix.len()].to_owned()
    }

    fn kind(&self, interpreter: &mut Interpreter) -> Option<FileKind> {
        let resolved = interpreter.host.resolve(&self.path);
        interpreter.host.stat(&resolved).map(|stat| stat.kind)
    }
}

fn path_argument(
    interpreter: &mut Interpreter,
    args: &Args,
    index: usize,
) -> Result<String, Error> {
    match args.positional.get(index) {
        Some(value) => path_text(interpreter, value),
        None => Err(Error::type_error("missing path argument")),
    }
}

impl Native for PathObject {
    fn type_name(&self) -> &str {
        "PosixPath"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn instance_of(&self, class: &str) -> bool {
        matches!(
            class,
            "PurePath" | "Path" | "PosixPath" | "PurePosixPath" | "PathLike"
        )
    }

    fn to_str(&self, _: &mut Interpreter) -> Result<Option<String>, Error> {
        Ok(Some(self.path.clone()))
    }

    fn repr(&self, _: &mut Interpreter) -> Result<String, Error> {
        Ok(format!("PosixPath({})", crate::format::quote(&self.path)))
    }

    fn key_equal(&self, other: &dyn Native) -> bool {
        other
            .as_any()
            .downcast_ref::<PathObject>()
            .is_some_and(|other| other.path == self.path)
    }

    fn key_hash(&self, state: &mut dyn std::hash::Hasher) {
        state.write(self.path.as_bytes());
    }

    fn binary(
        &self,
        interpreter: &mut Interpreter,
        operator: Operator,
        other: &Value,
        reflected: bool,
    ) -> Result<Option<Value>, Error> {
        let other_text = match other.plain() {
            Value::Str(text) => Some(text.to_string()),
            Value::Native(object) => object
                .as_any()
                .downcast_ref::<PathObject>()
                .map(|path| path.path.clone()),
            _ => None,
        };
        let Some(other_text) = other_text else {
            return Ok(match operator {
                Operator::Eq => Some(Value::Bool(false)),
                Operator::NotEq => Some(Value::Bool(true)),
                _ => None,
            });
        };
        let _ = interpreter;
        Ok(match operator {
            Operator::TrueDiv => Some(Value::native(if reflected {
                PathObject::new(&other_text).join(&self.path)
            } else {
                self.join(&other_text)
            })),
            Operator::Eq => Some(Value::Bool(
                matches!(other.plain(), Value::Native(_))
                    && PathObject::new(&other_text).path == self.path,
            )),
            Operator::NotEq => Some(Value::Bool(
                !(matches!(other.plain(), Value::Native(_))
                    && PathObject::new(&other_text).path == self.path),
            )),
            Operator::Lt => Some(Value::Bool(self.path < other_text)),
            Operator::Gt => Some(Value::Bool(self.path > other_text)),
            _ => None,
        })
    }

    fn get_attribute(
        &self,
        interpreter: &mut Interpreter,
        _: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        let this = self.clone();
        Ok(Some(match name {
            "name" => Value::str(this.name()),
            "stem" => Value::str(this.stem()),
            "suffix" => Value::str(this.suffix()),
            "suffixes" => {
                let name = this.name();
                let trimmed = name.trim_start_matches('.');
                let parts: Vec<&str> = trimmed.split('.').collect();
                Value::list(
                    parts[1..]
                        .iter()
                        .map(|part| Value::str(format!(".{part}")))
                        .collect(),
                )
            }
            "parent" => Value::native(this.parent()),
            "parents" => {
                let mut parents = Vec::new();
                let mut current = this.parent();
                loop {
                    parents.push(Value::native(current.clone()));
                    let next = current.parent();
                    if next.path == current.path {
                        break;
                    }
                    current = next;
                }
                if this.path == "." || this.path == "/" {
                    parents.clear();
                }
                Value::tuple(parents)
            }
            "parts" => {
                let mut parts: Vec<Value> = Vec::new();
                if this.path.starts_with('/') {
                    parts.push(Value::str("/"));
                }
                parts.extend(
                    this.path
                        .split('/')
                        .filter(|part| !part.is_empty() && *part != ".")
                        .map(Value::str),
                );
                Value::tuple(parts)
            }
            "anchor" | "root" => Value::str(if this.path.starts_with('/') { "/" } else { "" }),
            "drive" => Value::str(""),
            "exists" => Value::builtin("exists", move |interpreter, _| {
                Ok(Value::Bool(this.kind(interpreter).is_some()))
            }),
            "is_file" => Value::builtin("is_file", move |interpreter, _| {
                Ok(Value::Bool(this.kind(interpreter) == Some(FileKind::File)))
            }),
            "is_dir" => Value::builtin("is_dir", move |interpreter, _| {
                Ok(Value::Bool(
                    this.kind(interpreter) == Some(FileKind::Directory),
                ))
            }),
            "is_symlink" => Value::builtin("is_symlink", |_, _| Ok(Value::Bool(false))),
            "is_absolute" => Value::builtin("is_absolute", move |_, _| {
                Ok(Value::Bool(this.path.starts_with('/')))
            }),
            "as_posix" | "__fspath__" | "__str__" => {
                Value::builtin(name, move |_, _| Ok(Value::str(this.path.as_str())))
            }
            "resolve" | "absolute" | "expanduser" => Value::builtin(name, move |interpreter, _| {
                let expanded = crate::host::expand_home(&this.path);
                Ok(PathObject::value(&abspath(interpreter, &expanded)))
            }),
            "joinpath" => Value::builtin("joinpath", move |interpreter, args| {
                let mut result = this.clone();
                for index in 0..args.positional.len() {
                    let part = path_argument(interpreter, &args, index)?;
                    result = result.join(&part);
                }
                Ok(Value::native(result))
            }),
            "with_suffix" => Value::builtin("with_suffix", move |interpreter, args| {
                let suffix = path_argument(interpreter, &args, 0)?;
                let stem = this.stem();
                Ok(Value::native(
                    this.parent().join(&format!("{stem}{suffix}")),
                ))
            }),
            "with_name" => Value::builtin("with_name", move |interpreter, args| {
                let name = path_argument(interpreter, &args, 0)?;
                Ok(Value::native(this.parent().join(&name)))
            }),
            "with_stem" => Value::builtin("with_stem", move |interpreter, args| {
                let stem = path_argument(interpreter, &args, 0)?;
                let suffix = this.suffix();
                Ok(Value::native(
                    this.parent().join(&format!("{stem}{suffix}")),
                ))
            }),
            "relative_to" => Value::builtin("relative_to", move |interpreter, args| {
                let base = PathObject::new(&path_argument(interpreter, &args, 0)?);
                let prefix = if base.path == "." {
                    String::new()
                } else {
                    format!("{}/", base.path)
                };
                if this.path == base.path {
                    return Ok(PathObject::value("."));
                }
                match this.path.strip_prefix(&prefix) {
                    Some(rest) if !prefix.is_empty() || !this.path.starts_with('/') => {
                        Ok(PathObject::value(rest))
                    }
                    _ => Err(Error::value_error(format!(
                        "'{}' is not in the subpath of '{}'",
                        this.path, base.path
                    ))),
                }
            }),
            "match" => Value::builtin("match", move |interpreter, args| {
                let pattern = path_argument(interpreter, &args, 0)?;
                let pattern_parts: Vec<&str> =
                    pattern.split('/').filter(|part| !part.is_empty()).collect();
                let parts: Vec<&str> = this
                    .path
                    .split('/')
                    .filter(|part| !part.is_empty())
                    .collect();
                if pattern_parts.len() > parts.len() {
                    return Ok(Value::Bool(false));
                }
                let tail = &parts[parts.len() - pattern_parts.len()..];
                for (pattern, part) in pattern_parts.iter().zip(tail) {
                    let regex = fancy_regex::Regex::new(&glob_regex(pattern))
                        .map_err(|error| Error::value_error(error.to_string()))?;
                    if !regex.is_match(part).unwrap_or(false) {
                        return Ok(Value::Bool(false));
                    }
                }
                Ok(Value::Bool(true))
            }),
            "glob" | "rglob" => {
                let recursive = name == "rglob";
                Value::builtin(name, move |interpreter, args| {
                    let pattern = path_argument(interpreter, &args, 0)?;
                    let pattern = if recursive {
                        format!("**/{pattern}")
                    } else {
                        pattern
                    };
                    let full = if this.path == "." {
                        pattern
                    } else {
                        join_two(&this.path, &pattern)
                    };
                    let found = glob(interpreter, &full, true)?;
                    Ok(Value::iterator(
                        found.into_iter().map(|path| PathObject::value(&path)),
                    ))
                })
            }
            "iterdir" => Value::builtin("iterdir", move |interpreter, _| {
                let resolved = interpreter.host.resolve(&this.path);
                let names = interpreter
                    .host
                    .list(&resolved)
                    .map_err(|error| Error::os_error(&error, &this.path))?;
                Ok(Value::iterator(
                    names
                        .into_iter()
                        .map(|name| Value::native(this.join(&name))),
                ))
            }),
            "read_text" => Value::builtin("read_text", move |interpreter, _| {
                let resolved = interpreter.host.resolve(&this.path);
                let data = interpreter
                    .host
                    .read(&resolved)
                    .map_err(|error| Error::os_error(&error, &this.path))?;
                Ok(Value::str(String::from_utf8_lossy(&data).into_owned()))
            }),
            "read_bytes" => Value::builtin("read_bytes", move |interpreter, _| {
                let resolved = interpreter.host.resolve(&this.path);
                let data = interpreter
                    .host
                    .read(&resolved)
                    .map_err(|error| Error::os_error(&error, &this.path))?;
                Ok(Value::Bytes(data.into()))
            }),
            "write_text" => Value::builtin("write_text", move |interpreter, args| {
                let text = args
                    .positional
                    .first()
                    .and_then(|text| text.as_str().map(str::to_owned))
                    .unwrap_or_default();
                let resolved = interpreter.host.resolve(&this.path);
                interpreter
                    .host
                    .write(&resolved, text.as_bytes(), false)
                    .map_err(|error| Error::os_error(&error, &this.path))?;
                Ok(Value::Int(text.len() as i64))
            }),
            "open" => Value::builtin("open", move |interpreter, mut args| {
                let mode = args
                    .take(0, "mode")
                    .and_then(|mode| mode.as_str().map(str::to_owned))
                    .unwrap_or_else(|| "r".into());
                crate::builtins::open_file(interpreter, &this.path, &mode, false)
            }),
            "mkdir" => Value::builtin("mkdir", move |interpreter, mut args| {
                let exist_ok = args
                    .take_keyword("exist_ok")
                    .is_some_and(|value| matches!(value.plain(), Value::Bool(true)));
                args.take_keyword("parents");
                let resolved = interpreter.host.resolve(&this.path);
                if interpreter.host.stat(&resolved).is_some() {
                    if exist_ok {
                        return Ok(Value::None);
                    }
                    return Err(Error::os_error(
                        &std::io::ErrorKind::AlreadyExists.into(),
                        &this.path,
                    ));
                }
                make_dirs(interpreter, &resolved)?;
                Ok(Value::None)
            }),
            "touch" => Value::builtin("touch", move |interpreter, _| {
                let resolved = interpreter.host.resolve(&this.path);
                if interpreter.host.stat(&resolved).is_none() {
                    interpreter
                        .host
                        .write(&resolved, &[], false)
                        .map_err(|error| Error::os_error(&error, &this.path))?;
                }
                Ok(Value::None)
            }),
            "unlink" => Value::builtin("unlink", move |interpreter, _| {
                remove(interpreter, Args::new(vec![Value::str(this.path.as_str())]))
            }),
            "stat" => Value::builtin("stat", move |interpreter, _| {
                stat(interpreter, Args::new(vec![Value::str(this.path.as_str())]))
            }),
            _ => {
                let _ = interpreter;
                return Ok(None);
            }
        }))
    }
}

pub fn pathlib() -> Vec<(&'static str, Value)> {
    fn make(interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
        let mut path = PathObject::new(".");
        for value in &args.positional {
            let part = path_text(interpreter, value)?;
            path = path.join(&part);
        }
        Ok(Value::native(path))
    }
    let class = Value::native(PathClass);
    vec![
        ("Path", class.clone()),
        ("PosixPath", class.clone()),
        ("PurePath", class.clone()),
        ("PurePosixPath", class),
        ("_make", function("_make", make)),
    ]
}

/// The `pathlib.Path` class: callable, and usable with `isinstance`.
struct PathClass;

impl Native for PathClass {
    fn type_name(&self) -> &str {
        "type"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn call(
        &self,
        interpreter: &mut Interpreter,
        _: &Value,
        args: Args,
    ) -> Result<Option<Value>, Error> {
        let mut path = PathObject::new(".");
        for value in &args.positional {
            let part = path_text(interpreter, value)?;
            path = path.join(&part);
        }
        Ok(Some(Value::native(path)))
    }

    fn instance_of(&self, class: &str) -> bool {
        class == "callable"
    }

    fn check_instance(&self, value: &Value) -> Option<bool> {
        Some(value.as_native::<PathObject>().is_some())
    }

    fn get_attribute(
        &self,
        interpreter: &mut Interpreter,
        _: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        Ok(match name {
            "cwd" => Some(Value::builtin("cwd", |interpreter, _| {
                Ok(PathObject::value(&interpreter.host.cwd().to_string_lossy()))
            })),
            "home" => Some(Value::builtin("home", |_, _| {
                Ok(PathObject::value(&crate::host::expand_home("~")))
            })),
            "__name__" => Some(Value::str("Path")),
            // A method taken from the class, like `Path.absolute(path)`, gets its path as the
            // first argument.
            _ => {
                let probe = Value::native(PathObject::new("."));
                if PathObject::new(".")
                    .get_attribute(interpreter, &probe, name)?
                    .is_none()
                {
                    return Ok(None);
                }
                let method = name.to_owned();
                Some(Value::builtin(name, move |interpreter, mut args| {
                    if args.positional.is_empty() {
                        return Err(Error::type_error(format!(
                            "Path.{method}() needs a path as its first argument"
                        )));
                    }
                    let path = args.positional.remove(0);
                    let path = match path.as_native::<PathObject>() {
                        Some(_) => path,
                        None => Value::native(PathObject::new(&path_text(interpreter, &path)?)),
                    };
                    let bound = interpreter.get_attribute(&path, &method)?;
                    interpreter.call(&bound, args)
                }))
            }
        })
    }
}

impl std::fmt::Display for PathObject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.path)
    }
}
