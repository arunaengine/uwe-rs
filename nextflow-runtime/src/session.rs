//! The host of a run: the host of the caller, with the functions of plugins.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;
use std::rc::Rc;

use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::{Error, FileMetadata, Host};
use nextflow_evaluator::lowered::{Module, Process};
use nextflow_evaluator::value::{PathValue, Value};

use crate::{Network, Options};

/// What a call of `log.info` and its siblings, or of `System.out.println`, prints; `None` for
/// another call. `log.debug` prints nothing.
pub(crate) fn printed(namespace: &str, name: &str, arguments: &[Value]) -> Option<String> {
    match (namespace, name, arguments) {
        ("log", "info" | "warn" | "error" | "debug", [message]) => {
            let prefix = match name {
                "warn" => "WARN: ",
                "error" => "ERROR ~ ",
                _ => "",
            };
            Some(match name {
                "debug" => String::new(),
                _ => format!("{prefix}{message}\n"),
            })
        }
        ("System.out" | "System.err", "println" | "print", [message]) => {
            let end = if name == "println" { "\n" } else { "" };
            Some(format!("{message}{end}"))
        }
        _ => None,
    }
}

pub(crate) struct Session<'a> {
    pub base: &'a mut dyn Host,
    pub options: &'a Options,
    /// The network once it has run, whose value channels completion handlers read.
    pub network: Option<&'a Network>,
    pub operator_directory: &'a mut Option<PathValue>,
}

impl Session<'_> {
    /// What plugin functions see of the run.
    fn plugin_context(&mut self) -> nextflow_plugins::Context<'_> {
        let workflow = crate::workflow::metadata(self.options, &self.base.launch_dir());
        nextflow_plugins::Context {
            host: &mut *self.base,
            config: &self.options.config,
            params: self.options.config.params(),
            project_dir: &self.options.project_dir,
            cli_params: &self.options.cli_params,
            workflow,
        }
    }

    /// Runs what the plugins of the config do before the script, like nf-schema's help;
    /// `false` when a plugin ended the run.
    pub(crate) fn start_plugins(&mut self) -> Result<bool, Error> {
        nextflow_plugins::start(&mut self.plugin_context())
    }

    /// Runs what the plugins of the config do at the end of a run, like nf-bactopia's summary.
    pub(crate) fn finish_plugins(&mut self, workflow: &Value) {
        let mut context = self.plugin_context();
        context.workflow = workflow.clone();
        nextflow_plugins::finish(&mut context);
    }
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

    fn open(&mut self, path: &PathValue) -> Result<Box<dyn std::io::Read>, Error> {
        self.base.open(path)
    }

    fn metadata(&mut self, path: &PathValue) -> Result<Option<FileMetadata>, Error> {
        self.base.metadata(path)
    }

    fn write(&mut self, path: &PathValue, bytes: &[u8]) -> Result<(), Error> {
        self.base.write(path, bytes)
    }

    fn copy_file(&mut self, source: &PathValue, target: &PathValue) -> Result<(), Error> {
        self.base.copy_file(source, target)
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
        if let Some(text) = printed(namespace, name, &arguments) {
            if !text.is_empty() {
                self.base.print(&text);
            }
            return Ok(Some(Value::Null));
        }
        // A factory outside a network, as a compiled run calls it, gives the list of its values.
        let factory = matches!(name, "fromPath" | "fromFilePairs")
            || nextflow_plugins::factory_plugin(name).is_some();
        if namespace == "channel" && factory && self.network.is_none() {
            let mut network = Network::default();
            let options = self.options;
            let mut builder = crate::build::Builder {
                network: &mut network,
                defer: false,
                written: Vec::new(),
                reads: Vec::new(),
                options,
                base: self,
                stub: options.stub,
                config: &options.config,
                output_dir: options.output_dir.clone(),
                exec_folder: options.exec_folder.clone().map(Rc::from),
            };
            let values = builder.factory_values(name, arguments)?;
            return Ok(Some(Value::list(values)));
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
        nextflow_plugins::call(plugin, name, arguments, &mut self.plugin_context())
    }

    fn plugin_method(
        &mut self,
        object: &Value,
        name: &str,
        arguments: Vec<Value>,
    ) -> Result<Option<Value>, Error> {
        nextflow_plugins::method(object, name, arguments, &mut self.plugin_context())
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
