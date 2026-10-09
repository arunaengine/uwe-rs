//! The host of a run: the host of the caller, with the functions of plugins.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;
use std::rc::Rc;

use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::{Error, FileMetadata, Host};
use nextflow_evaluator::lowered::{Module, Process};
use nextflow_evaluator::value::{PathValue, Value};

use crate::{Network, Options};

pub(crate) struct Session<'a> {
    pub base: &'a mut dyn Host,
    pub options: &'a Options,
    /// The network once it has run, whose value channels completion handlers read.
    pub network: Option<&'a Network>,
    pub operator_directory: &'a mut Option<PathValue>,
}

impl Host for Session<'_> {
    fn print(&mut self, text: &str) {
        self.base.print(text);
    }

    fn simulated(&self) -> bool {
        self.options.simulated || self.base.simulated()
    }

    fn launch_dir(&self) -> String {
        self.base.launch_dir()
    }

    fn operator_directory(&mut self) -> Result<PathValue, Error> {
        if let Some(directory) = self.operator_directory.as_ref() {
            return Ok(directory.clone());
        }
        let directory = self.base.operator_directory()?;
        *self.operator_directory = Some(directory.clone());
        Ok(directory)
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

    fn secret(&mut self, name: &str) -> Result<Option<String>, Error> {
        self.base.secret(name)
    }

    fn environment(&self, name: &str) -> Option<String> {
        self.base.environment(name)
    }

    fn list(&mut self, directory: &PathValue) -> Result<Vec<(String, bool)>, Error> {
        self.base.list(directory)
    }

    fn spread(&mut self, path: &PathValue) {
        self.base.spread(path);
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
        if let ("System.out" | "System.err", "println" | "print", [message]) =
            (namespace, name, arguments.as_slice())
        {
            let end = if name == "println" { "\n" } else { "" };
            self.base.print(&format!("{message}{end}"));
            return Ok(Some(Value::Null));
        }
        self.base.namespace_method(namespace, name, arguments)
    }

    /// `.val` of a value channel after the run is its value, as completion handlers read it.
    fn channel_property(&mut self, channel: &Value, name: &str) -> Result<Option<Value>, Error> {
        match (self.network, channel, name) {
            (Some(network), Value::Channel(channel), "val" | "value")
                if network.is_value(*channel) =>
            {
                Ok(Some(network.bound(*channel).unwrap_or(Value::Null)))
            }
            _ => self.base.channel_property(channel, name),
        }
    }

    /// `getVal()` of a value channel after the run gives its value, as completion handlers
    /// read it.
    fn operator(
        &mut self,
        source: &Value,
        name: &str,
        arguments: Vec<Value>,
    ) -> Result<Option<Value>, Error> {
        if let (Some(network), Value::Channel(channel), "getVal" | "getValue" | "val", []) =
            (self.network, source, name, arguments.as_slice())
            && network.is_value(*channel)
        {
            return Ok(Some(network.bound(*channel).unwrap_or(Value::Null)));
        }
        // Groovy's `println()` of any object prints an empty line.
        if let (Value::Channel(_), "println", []) = (source, name, arguments.as_slice()) {
            self.base.print("\n");
            return Ok(Some(Value::Null));
        }
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

    /// Validates with the everit validator of `nextflow-plugins`.
    fn validate_json(&mut self, schema: &str, instance: &str) -> Result<Option<String>, Error> {
        match nextflow_plugins::everit::violation(schema, instance) {
            Ok(violation) => Ok(violation.map(|violation| violation.to_string())),
            Err(message) => Err(Error::Thrown {
                exception: Rc::new(Exception::new("SchemaException", message)),
                span: Default::default(),
            }),
        }
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
