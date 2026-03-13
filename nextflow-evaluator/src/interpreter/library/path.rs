//! Methods of paths that do not read the file system, from Nextflow's `FilesEx` and the JDK.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_parser::Span;

use super::super::{Error, Interpreter};
use crate::number::Number;
use crate::value::{PathValue, Value};

pub(super) fn path_method(path: &Rc<PathValue>, name: &str, arguments: &[Value]) -> Option<Value> {
    let text = |value: &Value| match value {
        Value::String(text) | Value::GString(text) => Some(text.to_string()),
        Value::Path(other) => Some(other.uri()),
        _ => None,
    };
    let path_value = |path: PathValue| Some(Value::Path(Rc::new(path)));
    match (name, arguments) {
        ("getName", []) => Some(Value::string(path.name())),
        ("getBaseName", []) => Some(Value::string(path.base_name())),
        ("getBaseName", [Value::Number(times)]) => {
            let mut name = path.name().to_owned();
            for _ in 0..times.to_i64() {
                match name.rfind('.') {
                    Some(dot) => name.truncate(dot),
                    None => break,
                }
            }
            Some(Value::string(name))
        }
        ("getSimpleName", []) => Some(Value::string(path.simple_name())),
        ("getExtension", []) => Some(Value::string(path.extension())),
        // Java's `Path.startsWith` and `endsWith` compare whole name parts.
        ("startsWith" | "endsWith", [other]) => {
            let other = text(other)?;
            let own: Vec<&str> = path
                .path
                .split('/')
                .filter(|part| !part.is_empty())
                .collect();
            let wanted: Vec<&str> = other.split('/').filter(|part| !part.is_empty()).collect();
            let fits = if name == "startsWith" {
                (other.starts_with('/') == path.path.starts_with('/')) && own.starts_with(&wanted)
            } else {
                !other.starts_with('/') && own.ends_with(&wanted)
            };
            Some(Value::Boolean(fits && !wanted.is_empty()))
        }
        ("getParent", []) => Some(
            path.parent()
                .map_or(Value::Null, |p| Value::Path(Rc::new(p))),
        ),
        ("getScheme", []) => Some(Value::string(path.scheme())),
        ("toUriString", []) => Some(Value::string(path.uri())),
        // `java.net.URI`, which prints as its text.
        ("toUri", []) => Some(Value::string(path.uri())),
        ("toAbsolutePath" | "normalize" | "toRealPath" | "toPath" | "toFile", []) => {
            Some(Value::Path(path.clone()))
        }
        ("resolve", [other]) => path_value(path.resolve(&text(other)?)),
        ("resolveSibling", [other]) => path_value(path.resolve_sibling(&text(other)?)),
        _ => None,
    }
}

impl Interpreter<'_> {
    /// Methods of paths that read the file system through the host.
    pub(super) fn file_method(
        &mut self,
        path: &PathValue,
        name: &str,
        arguments: &[Value],
        span: Span,
    ) -> Result<Option<Value>, Error> {
        let host_error = |error: Error| error.or_at(span);
        match (name, arguments) {
            ("setText" | "write", [text]) => {
                let text = text.to_string();
                self.host.write(path, text.as_bytes()).map_err(host_error)?;
                return Ok(Some(Value::Null));
            }
            ("copyTo", [target]) => {
                let target = match target {
                    Value::Path(target) => (**target).clone(),
                    other => PathValue::parse(&other.to_string(), &self.host.launch_dir()),
                };
                let bytes = self.host.read(path).map_err(host_error)?;
                self.host.write(&target, &bytes).map_err(host_error)?;
                return Ok(Some(Value::Path(Rc::new(target))));
            }
            ("delete", []) => {
                let existed = self.host.delete(path).map_err(host_error)?;
                return Ok(Some(Value::Boolean(existed)));
            }
            ("mkdirs" | "mkdir", []) => {
                self.host.create_directories(path).map_err(host_error)?;
                return Ok(Some(Value::Boolean(true)));
            }
            ("listFiles", []) => {
                let entries = self.host.list(path).map_err(host_error)?;
                let mut files: Vec<PathValue> = entries
                    .into_iter()
                    .map(|(name, _)| path.resolve(&name))
                    .collect();
                files.sort_by(|a, b| a.path.cmp(&b.path));
                return Ok(Some(Value::list(
                    files
                        .into_iter()
                        .map(|file| Value::Path(Rc::new(file)))
                        .collect(),
                )));
            }
            ("countLines" | "countFasta" | "countFastq", []) => {
                let bytes = self.host.read(path).map_err(host_error)?;
                let text = String::from_utf8_lossy(&bytes);
                let count = match name {
                    "countLines" => text.lines().count(),
                    "countFasta" => text.lines().filter(|line| line.starts_with('>')).count(),
                    _ => text.lines().count() / 4,
                };
                return Ok(Some(Value::Number(Number::Long(count as i64))));
            }
            (_, []) => {}
            _ => return Ok(None),
        }
        Ok(Some(match name {
            "getText" | "readLines" => {
                let bytes = self.host.read(path).map_err(|error| error.or_at(span))?;
                let text = String::from_utf8_lossy(&bytes).into_owned();
                if name == "getText" {
                    Value::string(text)
                } else {
                    return self.string_method(&text, "readLines", &[], span);
                }
            }
            "exists" | "isFile" | "isDirectory" | "size" | "isEmpty" => {
                let metadata = self
                    .host
                    .metadata(path)
                    .map_err(|error| error.or_at(span))?;
                match name {
                    "exists" => Value::Boolean(metadata.is_some()),
                    "isFile" => Value::Boolean(metadata.is_some_and(|file| !file.directory)),
                    "isDirectory" => Value::Boolean(metadata.is_some_and(|file| file.directory)),
                    "size" => {
                        Value::Number(Number::Long(metadata.map_or(0, |file| file.size as i64)))
                    }
                    // Nextflow's `isEmpty`: missing, or no bytes.
                    _ => Value::Boolean(metadata.is_none_or(|file| file.size == 0)),
                }
            }
            _ => return Ok(None),
        }))
    }
}
