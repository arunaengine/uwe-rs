//! The host of a run: the host of the caller, with the functions of plugins.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;
use std::rc::Rc;

use nextflow_evaluator::interpreter::{Error, FileMetadata, Host};
use nextflow_evaluator::lowered::{Module, Process};
use nextflow_evaluator::value::{PathValue, Value};

use crate::Options;

pub(crate) struct Session<'a> {
    pub base: &'a mut dyn Host,
    pub options: &'a Options,
}

impl Host for Session<'_> {
    fn print(&mut self, text: &str) {
        self.base.print(text);
    }

    fn launch_dir(&self) -> String {
        self.base.launch_dir()
    }

    fn download(&mut self, path: &PathValue) -> Result<PathBuf, Error> {
        self.base.download(path)
    }

    fn read(&mut self, path: &PathValue) -> Result<Vec<u8>, Error> {
        self.base.read(path)
    }

    fn metadata(&mut self, path: &PathValue) -> Result<Option<FileMetadata>, Error> {
        self.base.metadata(path)
    }

    fn write(&mut self, path: &PathValue, bytes: &[u8]) -> Result<(), Error> {
        self.base.write(path, bytes)
    }

    fn delete(&mut self, path: &PathValue) -> Result<bool, Error> {
        self.base.delete(path)
    }

    fn create_directories(&mut self, path: &PathValue) -> Result<(), Error> {
        self.base.create_directories(path)
    }

    fn environment(&self, name: &str) -> Option<String> {
        self.base.environment(name)
    }

    fn list(&mut self, directory: &PathValue) -> Result<Vec<(String, bool)>, Error> {
        self.base.list(directory)
    }

    fn namespace_method(
        &mut self,
        namespace: &str,
        name: &str,
        arguments: Vec<Value>,
    ) -> Result<Option<Value>, Error> {
        if let ("log", "info" | "warn" | "error" | "debug", [message]) =
            (namespace, name, arguments.as_slice())
        {
            let prefix = match name {
                "warn" => "WARN: ",
                "error" => "ERROR ~ ",
                _ => "",
            };
            if name != "debug" {
                self.base.print(&format!("{prefix}{message}\n"));
            }
            return Ok(Some(Value::Null));
        }
        self.base.namespace_method(namespace, name, arguments)
    }

    fn operator(
        &mut self,
        source: &Value,
        name: &str,
        arguments: Vec<Value>,
    ) -> Result<Option<Value>, Error> {
        self.base.operator(source, name, arguments)
    }

    fn plugin_function(
        &mut self,
        plugin: &str,
        name: &str,
        arguments: Vec<Value>,
    ) -> Result<Value, Error> {
        let mut context = nextflow_plugins::Context {
            host: &mut *self.base,
            config: &self.options.config,
            params: self.options.config.params(),
            project_dir: &self.options.project_dir,
        };
        nextflow_plugins::call(plugin, name, arguments, &mut context)
    }

    fn call_process(
        &mut self,
        module: &Rc<Module>,
        process: &Rc<Process>,
        name: &str,
        arguments: Vec<Value>,
    ) -> Result<Value, Error> {
        self.base.call_process(module, process, name, arguments)
    }
}
