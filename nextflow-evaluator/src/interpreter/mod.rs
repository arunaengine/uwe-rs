//! The interpreter: runs the lowered tree with Groovy's semantics.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod access;
mod call;
mod dataflow;
mod expression;
pub mod glob;
mod library;
mod statement;
mod task;
mod template;

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::rc::Rc;

use nextflow_parser::Span;

use crate::exception::Exception;
use crate::lowered::{Code, Module, Place, Process};
use crate::value::{Closure, PathValue, Value};

pub use library::{parse_json, parse_yaml};
pub use task::{
    RenderedOutput, RenderedPart, RenderedTask, TaskDirective, TaskFrame, normalise_script,
};

/// What the interpreter needs from its surroundings.
pub trait Host {
    /// Writes text to standard output, as `print` and `println` do.
    fn print(&mut self, text: &str);

    /// The directory the run started in, which relative paths in `file()` start from.
    fn launch_dir(&self) -> String {
        std::env::current_dir()
            .map(|dir| dir.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "/".into())
    }

    /// A local copy of a remote file, like one on `https://`. The default has none.
    fn download(&mut self, path: &PathValue) -> Result<PathBuf, Error> {
        Err(unsupported(
            format!("reading remote files like {}", path.uri()),
            Span::default(),
        ))
    }

    /// The bytes of a file. The default reads local files and downloads remote ones.
    fn read(&mut self, path: &PathValue) -> Result<Vec<u8>, Error> {
        let local = match path.prefix {
            Some(_) => self.download(path)?,
            None => PathBuf::from(&path.path),
        };
        std::fs::read(local).map_err(|error| file_error(path, &error))
    }

    /// Whether a file exists: `None` when it does not, else whether it is a directory and its
    /// size. The default looks at local files and downloads remote ones.
    fn metadata(&mut self, path: &PathValue) -> Result<Option<FileMetadata>, Error> {
        let local = match path.prefix {
            Some(_) => self.download(path)?,
            None => PathBuf::from(&path.path),
        };
        match std::fs::metadata(local) {
            Ok(metadata) => Ok(Some(FileMetadata {
                directory: metadata.is_dir(),
                size: metadata.len(),
            })),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(file_error(path, &error)),
        }
    }

    /// The entries of a directory: each name and whether it is a directory. The default lists
    /// local directories.
    fn list(&mut self, directory: &PathValue) -> Result<Vec<(String, bool)>, Error> {
        let local = local_path(directory)?;
        let entries = std::fs::read_dir(local).map_err(|error| file_error(directory, &error))?;
        let mut result = Vec::new();
        for entry in entries.flatten() {
            let is_directory = std::fs::metadata(entry.path()).is_ok_and(|m| m.is_dir());
            result.push((
                entry.file_name().to_string_lossy().into_owned(),
                is_directory,
            ));
        }
        Ok(result)
    }

    /// Writes a file, creating its directory. The default writes local files.
    fn write(&mut self, path: &PathValue, bytes: &[u8]) -> Result<(), Error> {
        let local = local_path(path)?;
        if let Some(parent) = local.parent() {
            std::fs::create_dir_all(parent).map_err(|error| file_error(path, &error))?;
        }
        std::fs::write(local, bytes).map_err(|error| file_error(path, &error))
    }

    /// Deletes a file; the result is whether it existed. The default deletes local files.
    fn delete(&mut self, path: &PathValue) -> Result<bool, Error> {
        match std::fs::remove_file(local_path(path)?) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(file_error(path, &error)),
        }
    }

    /// Creates a directory and its parents. The default creates local directories.
    fn create_directories(&mut self, path: &PathValue) -> Result<(), Error> {
        std::fs::create_dir_all(local_path(path)?).map_err(|error| file_error(path, &error))
    }

    /// A variable of the launch environment, as `env('HOME')` reads it.
    fn environment(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }

    /// A method of a namespace like `channel` or `log`; `None` when it has no such method.
    fn namespace_method(
        &mut self,
        _namespace: &str,
        _name: &str,
        _arguments: Vec<Value>,
    ) -> Result<Option<Value>, Error> {
        Ok(None)
    }

    /// An operator applied to a channel or to the outputs of a call; `None` when there is no
    /// such operator.
    fn operator(
        &mut self,
        _source: &Value,
        _name: &str,
        _arguments: Vec<Value>,
    ) -> Result<Option<Value>, Error> {
        Ok(None)
    }

    /// Publishes a channel as a workflow output, from the `publish:` section.
    fn publish(&mut self, name: &str, _channel: Value) -> Result<(), Error> {
        let message = format!("publishing the workflow output `{name}`");
        Err(unsupported(message, Span::default()))
    }

    /// Calls a function of a plugin, like `samplesheetToList` of `nf-schema`.
    fn plugin_function(
        &mut self,
        plugin: &str,
        name: &str,
        _arguments: Vec<Value>,
    ) -> Result<Value, Error> {
        let message = format!("the function `{name}` of the plugin `{plugin}`");
        Err(unsupported(message, Span::default()))
    }

    /// Calls a process with its input channels or values, as a workflow does; the result is
    /// the outputs of the call. `name` is the fully qualified name, like `RNASEQ:FASTQC`.
    fn call_process(
        &mut self,
        _module: &Rc<Module>,
        process: &Rc<Process>,
        _name: &str,
        _arguments: Vec<Value>,
    ) -> Result<Value, Error> {
        let message = format!("calling the process `{}`", process.name);
        Err(unsupported(message, Span::default()))
    }
}

/// What `metadata` tells about a file that exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileMetadata {
    pub directory: bool,
    pub size: u64,
}

/// The local file of a path; remote files cannot be written yet.
fn local_path(path: &PathValue) -> Result<PathBuf, Error> {
    match path.prefix {
        Some(_) => Err(unsupported(
            format!("writing remote files like {}", path.uri()),
            Span::default(),
        )),
        None => Ok(PathBuf::from(&path.path)),
    }
}

/// Java's `NoSuchFileException` for a missing file, else an `IOException`.
fn file_error(path: &PathValue, error: &std::io::Error) -> Error {
    let class = match error.kind() {
        std::io::ErrorKind::NotFound => "NoSuchFileException",
        _ => "IOException",
    };
    thrown(Exception::new(class, path.uri()), Span::default())
}

/// Why running code stopped before its end.
#[derive(Clone, Debug)]
pub enum Error {
    /// A Java exception or error. Scripts can catch exceptions with `try` and `catch`.
    Thrown {
        exception: Rc<Exception>,
        span: Span,
    },
    /// A feature that Nextflow supports but iunctus does not yet. Scripts cannot catch it.
    Unsupported { message: String, span: Span },
    /// An error with the file and position where it happened, like `main.nf:3:5`, once it
    /// leaves the scripts.
    Located {
        error: Box<Error>,
        location: Rc<str>,
    },
}

impl Error {
    pub fn span(&self) -> Span {
        match self {
            Error::Thrown { span, .. } | Error::Unsupported { span, .. } => *span,
            Error::Located { error, .. } => error.span(),
        }
    }

    /// Whether the cause is a feature that iunctus does not support yet.
    pub fn unsupported(&self) -> bool {
        match self {
            Error::Unsupported { .. } => true,
            Error::Thrown { .. } => false,
            Error::Located { error, .. } => error.unsupported(),
        }
    }

    /// The error with a position, if it has none yet, as errors from the host have.
    fn or_at(mut self, position: Span) -> Error {
        match &mut self {
            Error::Thrown { span, .. } | Error::Unsupported { span, .. } => {
                if *span == Span::default() {
                    *span = position;
                }
            }
            Error::Located { .. } => {}
        }
        self
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Thrown { exception, .. } => write!(f, "{exception}"),
            Error::Unsupported { message, .. } => write!(f, "Not supported yet: {message}"),
            Error::Located { error, location } => write!(f, "{location}: {error}"),
        }
    }
}

impl std::error::Error for Error {}

/// Runs functions and closures of lowered scripts.
pub struct Interpreter<'h> {
    host: &'h mut dyn Host,
    globals: HashMap<Rc<str>, Value>,
    /// The outputs of the last call of each process and workflow, by script and name, for `.out`.
    outputs: HashMap<(usize, Rc<str>), Value>,
    /// The names of the named workflows being called, outermost first.
    workflow_path: Vec<Rc<str>>,
    /// The variables of the task being evaluated, by name, which config closures can read.
    task_variables: Vec<(Rc<str>, Rc<RefCell<Value>>)>,
    /// The script of the statement where the error that is being raised happened.
    error_module: Option<Rc<Module>>,
}

impl<'h> Interpreter<'h> {
    pub fn new(host: &'h mut dyn Host) -> Self {
        Self {
            host,
            globals: HashMap::new(),
            outputs: HashMap::new(),
            workflow_path: Vec::new(),
            task_variables: Vec::new(),
            error_module: None,
        }
    }

    /// Writes text to standard output through the host, as `view` does.
    pub fn print(&mut self, text: &str) {
        self.host.print(text);
    }

    /// The host, for operators that read and write files.
    pub fn host(&mut self) -> &mut dyn Host {
        &mut *self.host
    }

    /// A global, like `workflow`.
    pub fn global(&self, name: &str) -> Option<Value> {
        self.globals.get(name).cloned()
    }

    /// Sets a built-in constant, like `params` or `workflow`.
    pub fn set_global(&mut self, name: &str, value: Value) {
        self.globals.insert(name.into(), value);
    }

    /// Calls a function of a script by name, as Groovy selects it by its arguments.
    pub fn call_function(
        &mut self,
        module: &Rc<Module>,
        name: &str,
        arguments: Vec<Value>,
    ) -> Result<Value, Error> {
        self.call_script_function(module, name, arguments, Span::default())
    }

    /// Evaluates code without parameters, like a config value or a `params.x = value` of a
    /// script, in a new frame of `module`.
    pub fn evaluate(&mut self, code: &Rc<Code>, module: &Rc<Module>) -> Result<Value, Error> {
        self.run(code, module, Vec::new(), Vec::new())
    }

    /// The error with the file and position where it happened, when the script is known.
    pub fn locate(&mut self, error: Error) -> Error {
        let module = self.error_module.take();
        let Some(module) = module else {
            return error;
        };
        let (Some(path), Some(source)) =
            (module.path.borrow().clone(), module.source.borrow().clone())
        else {
            return error;
        };
        if matches!(error, Error::Located { .. }) {
            return error;
        }
        let (line, column) =
            nextflow_parser::LineIndex::new(&source).line_column(error.span().start);
        Error::Located {
            error: Box::new(error),
            location: format!("{path}:{line}:{column}").into(),
        }
    }

    /// Calls a closure with Groovy's argument adaptation.
    pub fn call_closure(
        &mut self,
        closure: &Rc<Closure>,
        arguments: Vec<Value>,
    ) -> Result<Value, Error> {
        self.call_closure_value(closure, arguments, Span::default())
    }

    /// Calls a method of a value, as a script does with `value.name(arguments)`.
    pub fn invoke_method(
        &mut self,
        object: &Value,
        name: &str,
        arguments: Vec<Value>,
    ) -> Result<Value, Error> {
        self.call_method(object, name, arguments, Span::default())
    }
}

/// The variables of one running function or closure.
struct Frame {
    module: Rc<Module>,
    slots: Vec<Rc<RefCell<Value>>>,
    captured: Vec<Rc<RefCell<Value>>>,
}

impl Frame {
    fn cell(&self, place: Place) -> &Rc<RefCell<Value>> {
        match place {
            Place::Local(slot) => &self.slots[slot],
            Place::Captured(index) => &self.captured[index],
        }
    }

    fn get(&self, place: Place) -> Value {
        self.cell(place).borrow().clone()
    }

    fn set(&self, place: Place, value: Value) {
        *self.cell(place).borrow_mut() = value;
    }
}

/// Attaches a position to a Java exception thrown by value code.
fn thrown(exception: Exception, span: Span) -> Error {
    Error::Thrown {
        exception: Rc::new(exception),
        span,
    }
}

fn unsupported(message: impl Into<String>, span: Span) -> Error {
    Error::Unsupported {
        message: message.into(),
        span,
    }
}

/// Converts a value operation's result, adding the position of the operation.
trait AtSpan<T> {
    fn at(self, span: Span) -> Result<T, Error>;
}

impl<T> AtSpan<T> for Result<T, Exception> {
    fn at(self, span: Span) -> Result<T, Error> {
        self.map_err(|exception| thrown(exception, span))
    }
}
