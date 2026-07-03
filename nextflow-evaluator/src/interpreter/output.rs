//! The workflow outputs of the `output { }` block: their options, evaluated when the workflow
//! publishes a channel, and the publish statements of a `path` closure, like
//! `file >> "fastq/${meta.id}/"`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use crate::lowered::{Module, OutputDeclaration};
use crate::value::Value;

use super::{Error, Interpreter};

/// The publish statements of a `path` closure: each the file or files, and the target.
pub type PublishStatements = Vec<(Value, String)>;

/// The options of a workflow output, evaluated.
#[derive(Clone, Debug, Default)]
pub struct OutputOptions {
    /// Each option by its name, like `path`, `mode`, `enabled` or `overwrite`. The value of
    /// `path` is a folder below the output directory, or a closure.
    pub options: Vec<(Rc<str>, Value)>,
    /// The options of the index file, like `path`, `header` and `sep`, when it has one.
    pub index: Vec<(Rc<str>, Value)>,
}

impl OutputOptions {
    /// The value of an option.
    pub fn get(&self, name: &str) -> Option<&Value> {
        let found = self
            .options
            .iter()
            .rev()
            .find(|(option, _)| &**option == name);
        found.map(|(_, value)| value)
    }

    /// The value of an option of the index file.
    pub fn index(&self, name: &str) -> Option<&Value> {
        let found = self
            .index
            .iter()
            .rev()
            .find(|(option, _)| &**option == name);
        found.map(|(_, value)| value)
    }
}

impl Interpreter<'_> {
    /// The options of the workflow output `name` that the `output { }` block of the module
    /// declares; none when it declares no such output.
    pub(super) fn output_options(
        &mut self,
        module: &Rc<Module>,
        name: &str,
    ) -> Result<OutputOptions, Error> {
        let Some(declaration) = module.outputs.iter().find(|output| &*output.name == name) else {
            return Ok(OutputOptions::default());
        };
        let declaration: &OutputDeclaration = declaration;
        let mut options = OutputOptions::default();
        for (option, code) in &declaration.options {
            options
                .options
                .push((option.clone(), self.evaluate(code, module)?));
        }
        for (option, code) in &declaration.index {
            options
                .index
                .push((option.clone(), self.evaluate(code, module)?));
        }
        Ok(options)
    }

    /// Calls the `path` closure of a workflow output with a published value. Its publish
    /// statements, like `file >> 'fastq/'`, are the result, each as the file or files and
    /// the target; `None` when it has none and names a folder instead, which is then the value.
    pub fn call_output_path(
        &mut self,
        closure: &Value,
        value: Value,
    ) -> Result<(Value, Option<PublishStatements>), Error> {
        let Value::Closure(closure) = closure else {
            return Ok((closure.clone(), None));
        };
        let outer = self.publishing.replace(Vec::new());
        let result = self.call_closure_value(closure, vec![value], Default::default());
        let statements = std::mem::replace(&mut self.publishing, outer);
        let statements = statements.filter(|statements| !statements.is_empty());
        Ok((result?, statements))
    }
}
