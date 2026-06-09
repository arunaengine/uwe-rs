//! Methods of paths that do not read the file system, from Nextflow's `FilesEx` and the JDK.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_parser::Span;

use super::super::{Error, Interpreter};
use crate::number::Number;
use crate::split::{self, Splitter};
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
        // The name as a relative path.
        ("getFileName", []) => {
            let mut name = PathValue::parse(path.name(), "/");
            name.path = path.name().to_owned();
            name.relative = Some(path.name().to_owned());
            Some(Value::Path(Rc::new(name)))
        }
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
        ("toUri", []) => {
            let text = match path.prefix {
                Some(_) => path.uri(),
                None => format!("file://{}", path.path),
            };
            Some(super::uri(&text))
        }
        ("toAbsolutePath" | "toRealPath" | "getAbsoluteFile", []) if path.relative.is_some() => {
            path_value(PathValue {
                relative: None,
                ..(**path).clone()
            })
        }
        ("toAbsolutePath" | "normalize" | "toRealPath" | "toPath" | "toFile", []) => {
            Some(Value::Path(path.clone()))
        }
        ("isAbsolute", []) => Some(Value::Boolean(path.is_absolute())),
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
            // Records of a file, which is decompressed first when its name ends with `.gz`.
            ("splitCsv", [] | [Value::Map(_)]) => {
                let mut bytes = self.host.read(path).map_err(host_error)?;
                if path.name().ends_with(".gz") {
                    let mut decoded = Vec::new();
                    let mut decoder = flate2::read::MultiGzDecoder::new(bytes.as_slice());
                    std::io::Read::read_to_end(&mut decoder, &mut decoded).map_err(|error| {
                        let message = format!("Cannot decompress {}: {error}", path.uri());
                        let exception = crate::exception::Exception::new("ZipException", message);
                        super::super::thrown(exception, span)
                    })?;
                    bytes = decoded;
                }
                let text = String::from_utf8_lossy(&bytes);
                return Ok(Some(super::csv_records(&text, arguments.first())));
            }
            // The records of a file, as the operators of these names read them. With
            // `file: true`, or a folder, each chunk is a file.
            (
                "splitText" | "splitFasta" | "splitFastq" | "splitJson" | "countLines"
                | "countFasta" | "countFastq",
                [] | [Value::Map(_)],
            ) => {
                let options = arguments.first();
                let mut bytes = self.host.read(path).map_err(host_error)?;
                let decompress = split::option(options, "decompress").is_none_or(|v| v.truth());
                if path.name().ends_with(".gz") && decompress {
                    bytes = split::decompressed(&bytes, path)
                        .map_err(|exception| super::super::thrown(exception, span))?;
                }
                let text = String::from_utf8_lossy(&bytes);
                let chunks = Splitter::new(name, options)
                    .chunks(&text)
                    .map_err(|exception| super::super::thrown(exception, span))?;
                if name.starts_with("count") {
                    return Ok(Some(Value::Number(Number::Long(chunks.len() as i64))));
                }
                let Some(files) = split::option(options, "file").filter(Value::truth) else {
                    return Ok(Some(Value::list(chunks)));
                };
                // As Nextflow's `AbstractTextSplitter`: a folder gets chunks named after the
                // file, another path is the name of the chunks, and so is a text, in a folder
                // of the file below `work/tmp`.
                let source = path.name().strip_suffix(".gz").unwrap_or(path.name());
                let (directory, named) = match files {
                    Value::Path(target) => {
                        let metadata = self.host.metadata(&target).map_err(host_error)?;
                        match target.parent() {
                            Some(parent) if !metadata.is_some_and(|m| m.directory) => {
                                (parent, target.name().to_owned())
                            }
                            _ => ((*target).clone(), source.to_owned()),
                        }
                    }
                    Value::String(text) | Value::GString(text) => (
                        chunk_directory(path, &self.host.launch_dir()),
                        text.to_string(),
                    ),
                    _ => (
                        chunk_directory(path, &self.host.launch_dir()),
                        source.to_owned(),
                    ),
                };
                let mut paths = Vec::new();
                for (index, chunk) in chunks.iter().enumerate() {
                    let file = directory.resolve(&chunk_name(&named, index + 1));
                    self.host
                        .write(&file, chunk.to_string().as_bytes())
                        .map_err(host_error)?;
                    paths.push(Value::Path(Rc::new(file)));
                }
                return Ok(Some(Value::list(paths)));
            }
            // Groovy's `withWriter`: the closure writes into a writer, whose text becomes the
            // file.
            ("withWriter", [.., Value::Closure(closure)]) => {
                let writer = Rc::new(std::cell::RefCell::new(crate::value::Object {
                    class: "java.io.BufferedWriter",
                    state: crate::value::State::Writer(String::new()),
                }));
                let result = self.call_closure_value(
                    closure,
                    vec![Value::Object(writer.clone())],
                    span,
                )?;
                let text = match &writer.borrow().state {
                    crate::value::State::Writer(text) => text.clone(),
                    _ => String::new(),
                };
                self.host.write(path, text.as_bytes()).map_err(host_error)?;
                return Ok(Some(result));
            }
            ("setText" | "write", [text]) => {
                let text = text.to_string();
                self.host.write(path, text.as_bytes()).map_err(host_error)?;
                return Ok(Some(Value::Null));
            }
            // Groovy's `Path.append(text)`, which makes the file if it is missing.
            ("append", [text @ (Value::String(_) | Value::GString(_))]) => {
                let mut bytes = match self.host.metadata(path).map_err(host_error)? {
                    Some(_) => self.host.read(path).map_err(host_error)?,
                    None => Vec::new(),
                };
                bytes.extend_from_slice(text.to_string().as_bytes());
                self.host.write(path, &bytes).map_err(host_error)?;
                return Ok(Some(Value::Null));
            }
            ("copyTo", [target]) => {
                let mut target = match target {
                    Value::Path(target) => (**target).clone(),
                    other => PathValue::parse(&other.to_string(), &self.host.launch_dir()),
                };
                // A file copied to a directory goes into it.
                let metadata = self.host.metadata(&target).map_err(host_error)?;
                if metadata.is_some_and(|metadata| metadata.directory) {
                    target = target.resolve(path.name());
                }
                let bytes = self.host.read(path).map_err(host_error)?;
                self.host.write(&target, &bytes).map_err(host_error)?;
                return Ok(Some(Value::Path(Rc::new(target))));
            }
            ("delete", []) => {
                let existed = self.host.delete(path).map_err(host_error)?;
                return Ok(Some(Value::Boolean(existed)));
            }
            // `mkdir` creates one directory in an existing one; both give false on failure.
            ("mkdirs" | "mkdir", []) => {
                let parent_exists = match path.parent() {
                    Some(parent) => self.host.metadata(&parent).ok().flatten().is_some(),
                    None => true,
                };
                let exists = self.host.metadata(path).ok().flatten();
                let created = match (name, exists) {
                    ("mkdir", Some(_)) => false,
                    ("mkdir", None) if !parent_exists => false,
                    (_, Some(metadata)) => metadata.directory,
                    _ => self.host.create_directories(path).is_ok(),
                };
                return Ok(Some(Value::Boolean(created)));
            }
            ("newInputStream" | "newReader", []) => {
                let bytes = self.host.read(path).map_err(host_error)?;
                return Ok(Some(super::stream(bytes)));
            }
            ("withInputStream" | "withReader", [Value::Closure(f)]) => {
                let bytes = self.host.read(path).map_err(host_error)?;
                let stream = super::stream(bytes);
                return Ok(Some(self.call_closure_value(f, vec![stream], span)?));
            }
            ("eachLine", [Value::Closure(f)]) => {
                let bytes = self.host.read(path).map_err(host_error)?;
                let stream = super::stream(bytes);
                return self
                    .call_method(&stream, "eachLine", vec![Value::Closure(f.clone())], span)
                    .map(Some);
            }
            ("eachFile" | "eachFileRecurse" | "eachDir", [.., Value::Closure(f)]) => {
                // The kind of files to visit: `FileType.FILES`, `DIRECTORIES` or `ANY`.
                let kind = match (name, arguments) {
                    ("eachDir", _) => "DIRECTORIES".to_owned(),
                    (_, [kind, _]) => kind.to_string(),
                    _ => "ANY".to_owned(),
                };
                let recurse = name == "eachFileRecurse";
                let mut pending = vec![path.clone()];
                while let Some(directory) = pending.pop() {
                    let mut entries = self.host.list(&directory).map_err(host_error)?;
                    entries.sort();
                    for (entry, is_directory) in entries {
                        let file = directory.resolve(&entry);
                        let wanted = match kind.as_str() {
                            "FILES" => !is_directory,
                            "DIRECTORIES" => is_directory,
                            _ => true,
                        };
                        if wanted {
                            let file = Value::Path(Rc::new(file.clone()));
                            self.call_closure_value(f, vec![file], span)?;
                        }
                        if recurse && is_directory {
                            pending.push(file);
                        }
                    }
                }
                return Ok(Some(Value::Null));
            }
            // The names of `listFiles()`, and none for a path that is not a directory.
            ("list", []) => {
                let metadata = self.host.metadata(path).map_err(host_error)?;
                let mut names: Vec<String> = match metadata {
                    Some(metadata) if metadata.directory => self
                        .host
                        .list(path)
                        .map_err(host_error)?
                        .into_iter()
                        .map(|(name, _)| name)
                        .collect(),
                    _ => Vec::new(),
                };
                names.sort();
                let names = names.into_iter().map(Value::string).collect();
                let list = Value::list(names);
                if let Value::List(list) = &list {
                    list.borrow_mut().kind = crate::value::ListKind::Array;
                }
                return Ok(Some(list));
            }
            // Like `File.listFiles()`, null for a path that is not a directory.
            ("listFiles", []) => {
                let metadata = self.host.metadata(path).map_err(host_error)?;
                if !metadata.is_some_and(|metadata| metadata.directory) {
                    return Ok(Some(Value::Null));
                }
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
            (_, []) => {}
            _ => return Ok(None),
        }
        Ok(Some(match name {
            "getBytes" => {
                let bytes = self.host.read(path).map_err(|error| error.or_at(span))?;
                super::bytes::bytes(bytes)
            }
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

/// The folder of the chunk files of a file, unique to it, below the `work/tmp` folder of the
/// launch folder, as Nextflow's `cacheableFile` makes one below its work folder.
fn chunk_directory(path: &PathValue, launch: &str) -> PathValue {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::hash::DefaultHasher::new();
    path.uri().hash(&mut hasher);
    PathValue::parse(&format!("work/tmp/{:016x}", hasher.finish()), launch)
}

/// The name of a chunk file, as Nextflow's `TextFileCollector` gives it: `name.1.fa` for the
/// first chunk named `name.fa`.
fn chunk_name(name: &str, index: usize) -> String {
    match name.rsplit_once('.') {
        Some((base, extension)) if !base.is_empty() => format!("{base}.{index}.{extension}"),
        _ => format!("{name}.{index}"),
    }
}
