//! Python packages without a stand-in (decision 19): they run in one Python process, which
//! the embedder starts, for example in a container. The interpreter sends each use of a
//! package object to the process as one request line, and gets one reply line. Objects of the
//! packages stay in the process as handles; plain values come back as values. The design is
//! in `docs/design/snakemake/python-packages.md`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod object;
mod protocol;
mod pypi;

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use serde_json::{Map, Value as Json};

pub use object::PackageObject;
pub use pypi::pypi_packages;

use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::value::Value;

/// The script of the Python process, which answers the requests of the interpreter.
pub const SERVER: &str = include_str!("server.py");

/// What the Python process needs to start.
#[derive(Clone, Debug)]
pub struct PythonStart {
    /// The server script, which the process runs with `python -u`.
    pub script: &'static str,
    /// The PyPI packages of the first import, to install before the script runs.
    pub packages: Vec<String>,
    /// The working directory of the interpreter, where the process runs.
    pub cwd: PathBuf,
}

/// A running Python process that answers each request line with one reply line.
pub trait PythonProcess {
    /// Sends one line and waits for the reply line. An error says why the process cannot
    /// answer, like the last lines it wrote to standard error before it ended.
    fn exchange(&mut self, line: &str) -> Result<String, String>;

    /// Ends the process and removes what it used.
    fn stop(&mut self);
}

/// Starts the Python process of a run, as the embedder can, for example in a container.
pub trait PythonStarter {
    /// Starts the process; an error says why it cannot start, like a missing container engine.
    fn start(&self, start: &PythonStart) -> Result<Box<dyn PythonProcess>, String>;
}

/// The Python process of the packages of one interpreter: how to start it, the running
/// process, and the handles that the interpreter no longer holds.
#[derive(Default)]
pub struct Packages {
    starter: RefCell<Option<Rc<dyn PythonStarter>>>,
    process: RefCell<Option<Box<dyn PythonProcess>>>,
    /// Counts the processes started, so that a handle of a stopped process is known.
    generation: Cell<u64>,
    released: RefCell<Vec<u64>>,
    /// The full dotted name that an import asks for while its packages are imported, so that
    /// `google.auth` installs `google-auth`, not `google`.
    wanted: RefCell<Option<String>>,
}

impl Packages {
    pub fn set_starter(&self, starter: Rc<dyn PythonStarter>) {
        *self.starter.borrow_mut() = Some(starter);
    }

    pub fn is_running(&self) -> bool {
        self.process.borrow().is_some()
    }

    /// Stops the process, when it runs: no more Python code of the run needs it.
    pub fn stop(&self) {
        let process = self.process.borrow_mut().take();
        if let Some(mut process) = process {
            let _ = process.exchange(r#"{"op":"stop"}"#);
            process.stop();
            self.generation.set(self.generation.get() + 1);
            self.released.borrow_mut().clear();
        }
    }

    /// Notes the full name of an import, unless an outer import did; whether it did.
    pub(crate) fn want(&self, module: &str) -> bool {
        let mut wanted = self.wanted.borrow_mut();
        if wanted.is_some() {
            return false;
        }
        *wanted = Some(module.to_owned());
        true
    }

    pub(crate) fn forget_wanted(&self) {
        *self.wanted.borrow_mut() = None;
    }

    /// Forgets a handle at the next request.
    fn release(&self, generation: u64, handle: u64) {
        if generation == self.generation.get()
            && let Ok(mut released) = self.released.try_borrow_mut()
        {
            released.push(handle);
        }
    }

    /// Imports a package module in the process, which starts at the first import.
    fn import(
        self: &Rc<Self>,
        interpreter: &mut Interpreter,
        module: &str,
    ) -> Result<Value, Error> {
        let wanted = self.wanted.borrow().clone();
        let full = wanted
            .filter(|wanted| wanted.starts_with(&format!("{module}.")))
            .unwrap_or_else(|| module.to_owned());
        let packages = pypi_packages(&full);
        if !self.is_running() {
            let starter = self.starter.borrow().clone();
            let top = module.split('.').next().unwrap_or(module);
            let Some(starter) = starter else {
                return Err(Error::stop(format!(
                    "the workflow imports the Python package {top}, which has no stand-in in iunctus; it runs in a Python container (decision 19), but this run has no container runtime"
                )));
            };
            let start = PythonStart {
                script: SERVER,
                packages: packages.clone(),
                cwd: interpreter.host.cwd(),
            };
            let process = starter.start(&start).map_err(|reason| {
                Error::stop(format!(
                    "the workflow imports the Python package {top}, which has no stand-in in iunctus; it runs in a Python container (decision 19), but the container cannot start: {reason}"
                ))
            })?;
            *self.process.borrow_mut() = Some(process);
            self.generation.set(self.generation.get() + 1);
        }
        let packages = Json::Array(packages.into_iter().map(Json::String).collect());
        self.call(
            "import",
            vec![
                ("module", Json::String(module.to_owned())),
                ("packages", packages),
            ],
        )
    }

    /// Sends a request and gives the value of its reply.
    fn call(self: &Rc<Self>, op: &str, fields: Vec<(&str, Json)>) -> Result<Value, Error> {
        let reply = self.request(op, fields)?;
        protocol::decode(self, &reply)
    }

    /// Sends a request with the handles to release, and gives the JSON value of its reply,
    /// or the exception of the package as an error.
    fn request(&self, op: &str, fields: Vec<(&str, Json)>) -> Result<Json, Error> {
        let mut request = Map::new();
        request.insert("op".into(), Json::String(op.to_owned()));
        for (name, value) in fields {
            request.insert(name.to_owned(), value);
        }
        let released = std::mem::take(&mut *self.released.borrow_mut());
        if !released.is_empty() {
            request.insert(
                "release".into(),
                Json::Array(released.into_iter().map(Json::from).collect()),
            );
        }
        let line = Json::Object(request).to_string();
        let reply = {
            let mut process = self.process.borrow_mut();
            let Some(process) = process.as_mut() else {
                return Err(Error::stop(
                    "the Python process of the packages has stopped, but workflow code still uses an object of a package",
                ));
            };
            process.exchange(&line).map_err(|reason| {
                Error::stop(format!(
                    "the Python process of the packages ended: {reason}"
                ))
            })?
        };
        let reply: Json = serde_json::from_str(&reply).map_err(|error| {
            Error::stop(format!(
                "the Python process of the packages gave a reply that is not JSON: {error}"
            ))
        })?;
        match reply {
            Json::Object(mut reply) => {
                if let Some(value) = reply.remove("value") {
                    return Ok(value);
                }
                match reply.remove("error") {
                    Some(error) => Err(protocol::exception(&error)),
                    None => Err(Error::stop(
                        "the Python process of the packages gave a reply without a value or an error",
                    )),
                }
            }
            _ => Err(Error::stop(
                "the Python process of the packages gave a reply that is not a JSON object",
            )),
        }
    }
}

impl Drop for Packages {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The path of an object of a package that is path-like, like `upath.UPath`: what its
/// `__fspath__` gives, or `None` for another value.
pub fn fspath(interpreter: &mut Interpreter, value: &Value) -> Result<Option<String>, Error> {
    if value.as_native::<PackageObject>().is_none() {
        return Ok(None);
    }
    let method = match interpreter.find_attribute(value, "__fspath__") {
        Ok(Some(method)) => method,
        Ok(None) => return Ok(None),
        Err(error) if error.is("AttributeError") => return Ok(None),
        Err(error) => return Err(error),
    };
    let path = interpreter.call(&method, crate::value::Args::default())?;
    interpreter.to_str(&path).map(Some)
}

impl Interpreter {
    /// Imports a module of a package without a stand-in in the Python process.
    pub(crate) fn import_package(&mut self, name: &str) -> Result<Value, Error> {
        let packages = self.packages.clone();
        let module = packages.import(self, name)?;
        self.modules.insert(name.to_owned(), module.clone());
        Ok(module)
    }
}
