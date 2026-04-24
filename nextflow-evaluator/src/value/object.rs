//! Objects of Java classes that iunctus stands in for, like `groovy.json.JsonSlurper`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use super::Closure;

/// An object of a stood-in class.
#[derive(Debug)]
pub struct Object {
    /// The fully qualified class name.
    pub class: &'static str,
    pub state: State,
}

#[derive(Debug)]
pub enum State {
    /// `groovy.json.JsonGenerator.Options`, a builder, or the `JsonGenerator` it builds.
    Json(JsonOptions),
    /// `groovy.json.JsonSlurper`
    JsonSlurper,
    /// `nextflow.script.WorkflowMetadata`, the `workflow` of a run.
    Workflow(Workflow),
    /// `groovy.text.SimpleTemplateEngine`
    TemplateEngine,
    /// `org.yaml.snakeyaml.Yaml`
    Yaml,
    /// `java.net.URI`, by its text.
    Uri(Rc<str>),
    /// An input stream or reader over bytes that were read at once, as `newInputStream()`,
    /// `GZIPInputStream` and readers give it, with the position of the next byte.
    Stream(Rc<[u8]>, usize),
    /// `nextflow.extension.GroupKey` from `groupKey(key, size)`: the key, which it stands for,
    /// with the size of its group for `groupTuple`.
    GroupKey(super::Value, i32),
    /// A template of the engine, with its text.
    Template(Rc<str>),
    /// `java.util.Locale`, by its tag like `en_US`.
    Locale(Rc<str>),
    /// `byte[]`, which like every Java array is only equal to itself.
    Bytes(Rc<[u8]>),
    /// `java.security.MessageDigest`, by its algorithm.
    Digest(&'static str),
    /// An object of a plugin class that is known by its text, like fgbio's `ReadStructure`.
    Text(Rc<str>),
    /// A Groovy class of the library of the pipeline, as its name gives it in code.
    Class(Rc<crate::lowered::LibraryClass>),
    /// A constant of an enum of the library; each constant is one object.
    EnumConstant {
        class: Rc<crate::lowered::LibraryClass>,
        name: Rc<str>,
        ordinal: usize,
    },
    /// `org.json.JSONObject`: a map, whose objects and arrays are `JSONObject` and
    /// `JSONArray` too.
    JsonObject(super::Value),
    /// `org.json.JSONArray`: a list.
    JsonArray(super::Value),
    /// `org.everit.json.schema.Schema`, by the JSON text of the schema.
    JsonSchema(Rc<str>),
    /// An object of a class of the library, with its fields.
    Instance {
        class: Rc<crate::lowered::LibraryClass>,
        fields: indexmap::IndexMap<Rc<str>, super::Value>,
    },
}

/// The `workflow` of a run: its properties, and the handlers that run when it ends.
#[derive(Debug)]
pub struct Workflow {
    /// A map of the properties, like `runName` and `manifest`.
    pub properties: super::Value,
    /// The closures of `workflow.onComplete` and `workflow.onError`, in order.
    pub on_complete: Vec<Rc<Closure>>,
    pub on_error: Vec<Rc<Closure>>,
}

/// How a JSON generator writes values.
#[derive(Clone, Debug, Default)]
pub struct JsonOptions {
    pub exclude_nulls: bool,
    /// Closures that turn an instance of a class, by simple name, into another value.
    pub converters: Vec<(Rc<str>, Rc<Closure>)>,
}
