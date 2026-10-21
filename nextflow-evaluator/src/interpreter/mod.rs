//! The interpreter: runs the lowered tree with Groovy's semantics.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod access;
mod call;
mod classes;
mod dataflow;
mod expression;
pub mod glob;
mod library;
mod output;
mod statement;
mod task;
mod template;

pub(crate) use access::instance_of;
pub use output::OutputOptions;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::PathBuf;
use std::rc::Rc;

use nextflow_parser::Span;

use crate::exception::Exception;
use crate::lowered::{Code, Module, Place, Process, Slot};
use crate::value::{Closure, PathValue, Value};

pub use library::{combinations, parse_json, parse_json_for_split, parse_yaml};
pub use task::{
    RenderedOutput, RenderedPart, RenderedTask, TaskDirective, TaskFrame, normalise_script,
};

thread_local! {
    /// The operator scratch folders that the default host made on this thread. They are
    /// removed when the thread ends, like the thread of a test or the main thread of a run.
    static SCRATCH: RefCell<Vec<tempfile::TempDir>> = const { RefCell::new(Vec::new()) };
}

/// What the interpreter needs from its surroundings.
pub trait Host {
    /// Writes text to standard output, as `print` and `println` do.
    fn print(&mut self, text: &str);

    /// Whether the run is simulated, so that its files are made up. Rules that Nextflow
    /// enforces on the content of files then let such files pass, like a CSV file without a
    /// header line.
    fn simulated(&self) -> bool {
        false
    }

    /// The directory the run started in, which relative paths in `file()` start from.
    fn launch_dir(&self) -> String {
        std::env::current_dir()
            .map(|dir| dir.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "/".into())
    }

    /// Reserves operator scratch. The default is a folder in the system temporary folder, which
    /// is removed when the thread ends; custom storage hosts may override it.
    fn operator_directory(&mut self) -> Result<PathValue, Error> {
        let directory = tempfile::Builder::new()
            .prefix("uwe-nextflow-")
            .tempdir()
            .map_err(|error| Error::Thrown {
                exception: Rc::new(Exception::new(
                    "IOException",
                    format!("cannot reserve operator scratch: {error}"),
                )),
                span: Span::default(),
            })?;
        let path = PathValue::parse(&directory.path().to_string_lossy(), "/");
        SCRATCH.with_borrow_mut(|folders| folders.push(directory));
        Ok(path)
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
            Ok(metadata) => Ok(Some(FileMetadata::of(&metadata))),
            // As in Java, a path below a file does not exist.
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                Ok(None)
            }
            Err(error) => Err(file_error(path, &error)),
        }
    }

    /// The entries of a directory: each name and whether it is a directory. The default lists
    /// local directories.
    fn list(&mut self, directory: &PathValue) -> Result<Vec<(String, bool)>, Error> {
        list_local(directory)
    }

    /// Notes that a file becomes an item of its own, as `flatten` makes one of each file in a
    /// list, so that the number of such files decides how many tasks follow. The engine marks
    /// the task that made the file when a wildcard matched it. The default does nothing.
    fn spread(&mut self, _path: &PathValue) {}

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

    fn secret(&mut self, name: &str) -> Result<Option<String>, Error> {
        crate::secrets::get(self, name)
    }

    /// A value channel bound to a value at once, even `null`, as a workflow emits a value
    /// that is not a channel; `None` when there are no channels.
    fn bound_channel(&mut self, _value: Value) -> Option<Value> {
        None
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

    /// Publishes a channel as a workflow output, from the `publish:` section, with the options
    /// that the `output { }` block gives it.
    fn publish(
        &mut self,
        name: &str,
        _channel: Value,
        _options: OutputOptions,
    ) -> Result<(), Error> {
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

    /// A property of a channel, like `ch.first().baseName`; `None` when the channel has no
    /// such property.
    fn channel_property(&mut self, _channel: &Value, _name: &str) -> Result<Option<Value>, Error> {
        Ok(None)
    }

    /// Validates the JSON text of an instance against the JSON text of a schema, as the
    /// everit validator of older Nextflow releases does: `None` when the instance is valid,
    /// else the JSON text of the violation, as `ValidationException.toJSON()` gives it.
    fn validate_json(&mut self, _schema: &str, _instance: &str) -> Result<Option<String>, Error> {
        Err(unsupported("validating JSON schemas", Span::default()))
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
    /// When the file last changed, in milliseconds since 1970, or 0 when not known.
    pub modified: i64,
    /// The Unix permission bits, or all of them when not known.
    pub mode: u32,
}

impl FileMetadata {
    /// The metadata of a local file.
    pub fn of(metadata: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let modified = (metadata.modified().ok())
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |since| since.as_millis() as i64);
        FileMetadata {
            directory: metadata.is_dir(),
            size: metadata.len(),
            modified,
            mode: metadata.permissions().mode(),
        }
    }
}

/// The entries of a local directory: each name and whether it is a directory.
pub fn list_local(directory: &PathValue) -> Result<Vec<(String, bool)>, Error> {
    let local = local_path(directory)?;
    let entries = match std::fs::read_dir(local) {
        Ok(entries) => entries,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(Vec::new());
        }
        Err(error) => return Err(file_error(directory, &error)),
    };
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

    /// The Java exception, if the error is one, also when it has a location.
    pub fn exception(&self) -> Option<&Rc<Exception>> {
        match self {
            Error::Thrown { exception, .. } => Some(exception),
            Error::Unsupported { .. } => None,
            Error::Located { error, .. } => error.exception(),
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
            Error::Thrown { exception, .. } => write!(f, "{}", exception.target()),
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
    /// The processes called so far, by name with the workflows that call them, which a run
    /// may call once each.
    invocations: HashSet<String>,
    /// While a typed output is computed: the values of its parts.
    task_outputs: Vec<Value>,
    /// Whether the script of the running code enables types, which changes some operators.
    types: bool,
    /// The variables of the task being evaluated, by name, which config closures can read.
    task_variables: Vec<(Rc<str>, Rc<RefCell<Value>>)>,
    /// The script of the statement where the error that is being raised happened.
    error_module: Option<Rc<Module>>,
    /// While the `path` closure of a workflow output runs: its publish statements, like
    /// `file >> 'fastq/'`, each as its source and target.
    publishing: Option<Vec<(Value, String)>>,
    /// How many functions and closures are running, to stop runaway recursion cleanly.
    depth: usize,
    /// The objects of the `with` and `tap` calls that run, whose properties and methods
    /// names that the script does not know reach, as the delegate of Groovy's closures.
    delegates: Vec<Value>,
}

impl<'h> Interpreter<'h> {
    pub fn new(host: &'h mut dyn Host) -> Self {
        Self {
            host,
            globals: HashMap::new(),
            outputs: HashMap::new(),
            workflow_path: Vec::new(),
            invocations: HashSet::new(),
            task_outputs: Vec::new(),
            types: false,
            task_variables: Vec::new(),
            error_module: None,
            publishing: None,
            depth: 0,
            delegates: Vec::new(),
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
        self.call_script_function(module, (name, false), arguments, Span::default())
    }

    /// Evaluates code without parameters, like a config value or a `params.x = value` of a
    /// script, in a new frame of `module`.
    pub fn evaluate(&mut self, code: &Rc<Code>, module: &Rc<Module>) -> Result<Value, Error> {
        self.run(code, module, Vec::new(), Vec::new())
    }

    /// The params that the code of a script reads: its own, from an include with
    /// `addParams(...)`, else those of the run.
    pub(super) fn params_of(&self, module: &Rc<Module>) -> Option<Value> {
        let own = module.params.borrow().clone();
        own.or_else(|| self.globals.get("params").cloned())
    }

    /// Starts a parameterized include by filling missing params, then running its statements.
    /// Runs unparameterized unconditional includes once each when their script loads.
    pub fn start_statements(&mut self, module: &Rc<Module>) -> Result<(), Error> {
        if module.started.replace(true) {
            return Ok(());
        }
        if let Some(code) = &module.initializer {
            self.evaluate(code, module)?;
        }
        let included: Vec<Rc<Module>> = {
            let links = module.links.borrow();
            let includes = module.includes.iter();
            includes
                .filter(|include| !include.conditional && !include.params)
                .filter_map(|include| links.get(&include.alias))
                .map(|link| link.module.clone())
                .collect()
        };
        for included in included {
            self.start_statements(&included)?;
        }
        Ok(())
    }

    pub(super) fn start_script(&mut self, module: &Rc<Module>) -> Result<(), Error> {
        let Some(params) = self.params_of(module) else {
            return Ok(());
        };
        for (path, code) in &module.parameters {
            let names: Vec<&str> = path.iter().map(|name| &**name).collect();
            if crate::value::tree::get(&params, &names).is_none() {
                let value = self.evaluate(code, module)?;
                crate::value::tree::assign(&params, path, value).map_err(|message| {
                    thrown(
                        crate::exception::Exception::illegal_argument(message),
                        code.span,
                    )
                })?;
            }
        }
        if let Some(code) = &module.initializer {
            self.evaluate(code, module)?;
        }
        Ok(())
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
        // An operator calls the closure; an error of the call itself is at the closure.
        let result = self.call_closure_value(closure, arguments, Span::default());
        if let Err(error) = &result
            && !matches!(error, Error::Located { .. })
            && error.span() == Span::default()
            && self.error_module.is_none()
        {
            self.error_module = Some(closure.module.clone());
            return result.map_err(|error| error.or_at(closure.code.span));
        }
        result
    }

    /// A value `as` a class, like `value as Integer`.
    pub fn cast_value(&mut self, value: Value, class: &str) -> Result<Value, Error> {
        self.cast(value, class, Span::default())
    }

    /// Reads a property of a value, as `value.name` does.
    pub fn read_property(&mut self, object: &Value, name: &str) -> Result<Value, Error> {
        self.property(object, name, Span::default())
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
    /// The variables of a task that its sections declare and that are not set yet, with their
    /// names. Reading one is an error, as reading a missing variable of Nextflow's task
    /// context is.
    unset: RefCell<Vec<(Slot, Rc<str>)>>,
}

impl Frame {
    fn cell(&self, place: Place) -> &Rc<RefCell<Value>> {
        match place {
            Place::Local(slot) => &self.slots[slot],
            Place::Captured(index) => &self.captured[index],
            Place::Module(index) => &self.module.variables[index],
        }
    }

    fn get(&self, place: Place) -> Value {
        self.cell(place).borrow().clone()
    }

    /// The name of a task variable that is not set yet.
    fn unset_name(&self, place: Place) -> Option<Rc<str>> {
        let Place::Local(slot) = place else {
            return None;
        };
        let unset = self.unset.borrow();
        let (_, name) = unset.iter().find(|(unset, _)| *unset == slot)?;
        Some(name.clone())
    }

    /// The value of a variable that code reads; an error for a task variable not set yet.
    fn read(&self, place: Place, span: Span) -> Result<Value, Error> {
        if let Some(name) = self.unset_name(place) {
            let message = format!("No such variable: {name}");
            return Err(thrown(
                Exception::new("MissingPropertyException", message),
                span,
            ));
        }
        Ok(self.get(place))
    }

    fn set(&self, place: Place, value: Value) {
        if let Place::Local(slot) = place {
            let mut unset = self.unset.borrow_mut();
            if !unset.is_empty() {
                unset.retain(|(unset, _)| *unset != slot);
            }
        }
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
