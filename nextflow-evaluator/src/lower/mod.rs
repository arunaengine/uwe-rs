//! Lowering: resolves the names of a syntax tree as Nextflow's compiler does, and reports the
//! same load-time errors, like "`x` is not defined".
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod expression;
mod names;
mod scope;
mod statement;

use std::collections::HashSet;
use std::rc::Rc;

use nextflow_parser::ast::{self, Item};
use nextflow_parser::{Diagnostic, Span};

use crate::lowered::{Code, Module, Parameter};
use scope::Scopes;

/// Lowers the functions of a script. Other script items are not supported yet.
pub fn lower_module(script: &ast::Script, source: &str) -> Result<Module, Vec<Diagnostic>> {
    let mut lowering = Lowering::new(source);
    for item in &script.items {
        if let Item::Function(function) = item {
            lowering
                .functions
                .insert(function.name.text.as_str().into());
        }
    }
    let mut module = Module::default();
    for item in &script.items {
        match item {
            Item::Function(function) => {
                let code = lowering.function(function);
                let index = module.functions.len();
                module.functions.push(Rc::new(code));
                let name: Rc<str> = function.name.text.as_str().into();
                module.function_names.entry(name).or_default().push(index);
            }
            Item::FeatureFlag { .. } => {}
            other => lowering.error("This script item is not supported yet", item_span(other)),
        }
    }
    if lowering.errors.is_empty() {
        Ok(module)
    } else {
        Err(lowering.errors)
    }
}

/// The state of lowering one script file.
pub(crate) struct Lowering<'a> {
    source: &'a str,
    scopes: Scopes,
    errors: Vec<Diagnostic>,
    /// The functions the script declares.
    functions: HashSet<Rc<str>>,
    /// Whether the code being lowered is inside the `${...}` of a GString.
    in_gstring: bool,
    /// Whether GStrings escape staged file names: in a process `script:` or `stub:`.
    escape_paths: bool,
    /// Whether the code belongs to a process, where `task` and `template` exist.
    in_process: bool,
    /// Whether assigning an undeclared name declares it, as in process and workflow sections.
    implicit_declarations: bool,
}

impl<'a> Lowering<'a> {
    pub(crate) fn new(source: &'a str) -> Self {
        Self {
            source,
            scopes: Scopes::default(),
            errors: Vec::new(),
            functions: HashSet::new(),
            in_gstring: false,
            escape_paths: false,
            in_process: false,
            implicit_declarations: false,
        }
    }

    fn error(&mut self, message: impl Into<String>, span: Span) {
        self.errors.push(Diagnostic::new(message, span));
    }

    fn function(&mut self, function: &ast::Function) -> Code {
        self.scopes.push_frame(false);
        let parameters = function
            .parameters
            .iter()
            .map(|parameter| {
                let default = parameter
                    .default
                    .as_ref()
                    .map(|value| self.expression(value));
                let slot = self.declare(&parameter.name);
                Parameter {
                    name: parameter.name.text.as_str().into(),
                    slot,
                    default,
                }
            })
            .collect();
        let body = self.statements(&function.body.statements);
        let layout = self.scopes.pop_frame();
        Code {
            name: function.name.text.as_str().into(),
            parameters,
            implicit_parameter: false,
            slot_count: layout.slot_count,
            captures: layout.captures,
            body,
            span: function.span,
        }
    }

    /// Declares a variable in the current block, reporting a name declared before.
    fn declare(&mut self, name: &ast::Name) -> usize {
        let (slot, earlier) = self.scopes.declare(&name.text, name.span);
        if earlier.is_some() {
            self.error(format!("`{}` is already declared", name.text), name.span);
        }
        slot
    }
}

fn item_span(item: &Item) -> Span {
    match item {
        Item::FeatureFlag { span, .. }
        | Item::Import { span, .. }
        | Item::Parameters { span, .. }
        | Item::LegacyParameter { span, .. }
        | Item::Record { span, .. }
        | Item::Enum { span, .. }
        | Item::Output { span, .. } => *span,
        Item::Include(include) => include.span,
        Item::Process(process) => process.span,
        Item::Workflow(workflow) => workflow.span,
        Item::Function(function) => function.span,
        Item::Agent(agent) => agent.span,
        Item::Statement(statement) => statement.span,
    }
}
