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
