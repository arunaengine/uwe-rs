//! What a Nextflow run notes while it plans, for the markers of the graph: the files that the
//! workflow reads or lists through the host, and the files that operators like `flatten` turn
//! into items of their own. The engine marks the tasks that made such files. Reads of the
//! `versions.yml` files of nf-core modules are left out: pipelines read them for the report of
//! tool versions, never to plan tasks.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;
use std::rc::Rc;

use nextflow_evaluator::interpreter::{Error, FileMetadata, Host};
use nextflow_evaluator::lowered::{Module, Process};
use nextflow_evaluator::value::{PathValue, Value};
use workflow_graph::Note;

/// The file in which nf-core modules give the versions of their tools.
const VERSIONS: &str = "versions.yml";

/// The host of the driver, with the notes of local files that the workflow reads, lists or
/// spreads.
pub(crate) struct Noting<'a> {
    pub host: &'a mut dyn Host,
    pub notes: &'a mut Vec<Note>,
}

impl Noting<'_> {
    fn note(&mut self, path: &PathValue, note: fn(PathBuf) -> Note) {
        if path.prefix.is_none() {
            self.notes.push(note(PathBuf::from(&path.path)));
        }
    }
}

impl Host for Noting<'_> {
    fn print(&mut self, text: &str) {
        self.host.print(text);
    }

    fn simulated(&self) -> bool {
        self.host.simulated()
    }

    fn launch_dir(&self) -> String {
        self.host.launch_dir()
    }

    fn download(&mut self, path: &PathValue) -> Result<PathBuf, Error> {
        self.host.download(path)
    }

    fn read(&mut self, path: &PathValue) -> Result<Vec<u8>, Error> {
        if path.name() != VERSIONS {
            self.note(path, Note::Read);
        }
        self.host.read(path)
    }

    fn metadata(&mut self, path: &PathValue) -> Result<Option<FileMetadata>, Error> {
        self.host.metadata(path)
    }

    fn list(&mut self, directory: &PathValue) -> Result<Vec<(String, bool)>, Error> {
        self.note(directory, Note::Read);
        self.host.list(directory)
    }

    fn spread(&mut self, path: &PathValue) {
        self.note(path, Note::Spread);
        self.host.spread(path);
    }

    fn write(&mut self, path: &PathValue, bytes: &[u8]) -> Result<(), Error> {
        self.host.write(path, bytes)
    }

    fn delete(&mut self, path: &PathValue) -> Result<bool, Error> {
        self.host.delete(path)
    }

    fn create_directories(&mut self, path: &PathValue) -> Result<(), Error> {
        self.host.create_directories(path)
    }

    fn environment(&self, name: &str) -> Option<String> {
        self.host.environment(name)
    }

    fn bound_channel(&mut self, value: Value) -> Option<Value> {
        self.host.bound_channel(value)
    }

    fn namespace_method(
        &mut self,
        namespace: &str,
        name: &str,
        arguments: Vec<Value>,
    ) -> Result<Option<Value>, Error> {
        self.host.namespace_method(namespace, name, arguments)
    }

    fn operator(
        &mut self,
        source: &Value,
        name: &str,
        arguments: Vec<Value>,
    ) -> Result<Option<Value>, Error> {
        self.host.operator(source, name, arguments)
    }

    fn publish(&mut self, name: &str, channel: Value) -> Result<(), Error> {
        self.host.publish(name, channel)
    }

    fn plugin_function(
        &mut self,
        plugin: &str,
        name: &str,
        arguments: Vec<Value>,
    ) -> Result<Value, Error> {
        self.host.plugin_function(plugin, name, arguments)
    }

    fn channel_property(&mut self, channel: &Value, name: &str) -> Result<Option<Value>, Error> {
        self.host.channel_property(channel, name)
    }

    fn validate_json(&mut self, schema: &str, instance: &str) -> Result<Option<String>, Error> {
        self.host.validate_json(schema, instance)
    }

    fn call_process(
        &mut self,
        module: &Rc<Module>,
        process: &Rc<Process>,
        name: &str,
        arguments: Vec<Value>,
    ) -> Result<Value, Error> {
        self.host.call_process(module, process, name, arguments)
    }
}

/// Tells the host of each file in a value, also inside lists and tuples, that it becomes an
/// item of its own.
pub(crate) fn spread(host: &mut dyn Host, value: &Value) {
    match value {
        Value::Path(path) => host.spread(path),
        Value::List(list) => {
            for item in &list.borrow().items {
                spread(host, item);
            }
        }
        _ => {}
    }
}
