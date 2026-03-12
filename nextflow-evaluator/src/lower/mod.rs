//! Lowering: resolves the names of a syntax tree as Nextflow's compiler does, and reports the
//! same load-time errors, like "`x` is not defined".
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod criteria;
mod expression;
mod names;
mod process;
mod scope;
mod statement;
mod workflow;

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use nextflow_parser::ast::{self, Item};
use nextflow_parser::{Diagnostic, Span};

use crate::lowered::{
    Code, DefinitionKind, Include, Module, Parameter, Statement, StatementKind, Workflow,
};
use scope::Scopes;

/// Tells lowering what a name that a script includes stands for.
pub trait Imports {
    /// The kind of `name` in the script that `source` names, relative to the including script,
    /// or the error to report.
    fn kind(&mut self, source: &str, name: &str) -> Result<DefinitionKind, String>;
}

/// For a script that includes nothing.
struct NoImports;

impl Imports for NoImports {
    fn kind(&mut self, source: &str, _: &str) -> Result<DefinitionKind, String> {
        Err(format!("Cannot include '{source}' without a module loader"))
    }
}

/// Lowers a script that includes nothing.
pub fn lower_module(script: &ast::Script, source: &str) -> Result<Module, Vec<Diagnostic>> {
    lower_script(script, source, &mut NoImports)
}

/// Lowers the value of one config setting. Outside closures, config code may only use
/// `params`, the directory constants, `secrets` and `env()`. Closures may also use the names of
/// scripts. In the `process` scope, a closure may use any other name too, like the task input
/// `meta`, which is looked up when the task runs.
pub fn lower_config_value(
    value: &ast::Expression,
    source: &str,
    process_scope: bool,
) -> Result<Code, Vec<Diagnostic>> {
    let mut lowering = Lowering::new(source);
    lowering.config = true;
    lowering.dynamic_names = process_scope;
    let code = lowering.expression_code("config", value, value.span);
    if lowering.errors.is_empty() {
        Ok(code)
    } else {
        Err(lowering.errors)
    }
}

/// Lowers an expression of a template, like `workflow.manifest.name` in `${...}`: every name
/// that is not a built-in constant is looked up when the template renders.
pub fn lower_template_value(
    value: &ast::Expression,
    source: &str,
) -> Result<Code, Vec<Diagnostic>> {
    let mut lowering = Lowering::new(source);
    lowering.config = true;
    lowering.dynamic_names = true;
    lowering.template = true;
    let code = lowering.expression_code("template", value, value.span);
    if lowering.errors.is_empty() {
        Ok(code)
    } else {
        Err(lowering.errors)
    }
}

/// Lowers a script: functions, processes, workflows, includes and `params.x = value`.
/// A script of statements only is the entry workflow; the parser rejects other mixes.
pub fn lower_script(
    script: &ast::Script,
    source: &str,
    imports: &mut dyn Imports,
) -> Result<Module, Vec<Diagnostic>> {
    let mut lowering = Lowering::new(source);
    let mut module = Module::default();
    lowering.declare_definitions(script, imports, &mut module);
    let statements: Vec<&ast::Statement> = script
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Statement(statement) => Some(statement),
            _ => None,
        })
        .collect();
    let declares = script.items.iter().any(|item| {
        !matches!(
            item,
            Item::Statement(_) | Item::Include(_) | Item::FeatureFlag { .. } | Item::Import { .. }
        )
    });
    for item in &script.items {
        match item {
            Item::Function(function) => {
                let code = lowering.function(function);
                let index = module.functions.len();
                module.functions.push(Rc::new(code));
                let name: Rc<str> = function.name.text.as_str().into();
                module.function_names.entry(name).or_default().push(index);
            }
            Item::Process(process) => {
                let process = lowering.process(process);
                module.processes.push(Rc::new(process));
            }
            Item::Workflow(workflow) => {
                let workflow = lowering.workflow(workflow);
                module.workflows.push(Rc::new(workflow));
            }
            Item::LegacyParameter { path, value, span } => {
                let path = path.iter().map(|name| name.text.as_str().into()).collect();
                let code = lowering.expression_code("params", value, *span);
                module.parameters.push((path, Rc::new(code)));
            }
            Item::Include(_) | Item::FeatureFlag { .. } | Item::Statement(_) => {}
            other => lowering.error("This script item is not supported yet", item_span(other)),
        }
    }
    if !declares && !statements.is_empty() {
        let workflow = lowering.implicit_workflow(&statements);
        module.workflows.push(Rc::new(workflow));
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
    /// The processes and workflows the script declares, and every name it includes.
    definitions: HashMap<Rc<str>, DefinitionKind>,
    /// Whether the code is the body of a workflow, where processes and workflows can be called.
    in_workflow: bool,
    /// Whether the code being lowered is inside the `${...}` of a GString.
    in_gstring: bool,
    /// Whether GStrings escape staged file names: in a process `script:` or `stub:`.
    escape_paths: bool,
    /// Whether the code belongs to a process, where `task` and `template` exist.
    in_process: bool,
    /// Whether assigning an undeclared name declares it, as in process and workflow sections.
    implicit_declarations: bool,
    /// Whether the code is a config value, which knows fewer names than a script.
    config: bool,
    /// Whether closures resolve unknown names when a task runs, as in the config `process` scope.
    dynamic_names: bool,
    /// Whether the code is a template expression, where every unknown name is resolved late.
    template: bool,
}

impl<'a> Lowering<'a> {
    pub(crate) fn new(source: &'a str) -> Self {
        Self {
            source,
            scopes: Scopes::default(),
            errors: Vec::new(),
            functions: HashSet::new(),
            definitions: HashMap::new(),
            in_workflow: false,
            in_gstring: false,
            escape_paths: false,
            in_process: false,
            implicit_declarations: false,
            config: false,
            dynamic_names: false,
            template: false,
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

    /// Records the names the script defines and includes, before lowering any code.
    fn declare_definitions(
        &mut self,
        script: &ast::Script,
        imports: &mut dyn Imports,
        module: &mut Module,
    ) {
        for item in &script.items {
            let (name, kind) = match item {
                Item::Function(function) => {
                    self.functions.insert(function.name.text.as_str().into());
                    continue;
                }
                Item::Process(process) => (&process.name, DefinitionKind::Process),
                Item::Workflow(workflow) => match &workflow.name {
                    Some(name) => (name, DefinitionKind::Workflow),
                    None => continue,
                },
                Item::Include(include) => {
                    for included in &include.names {
                        let alias = included.alias.as_ref().unwrap_or(&included.name);
                        if self.definitions.contains_key(alias.text.as_str()) {
                            self.error(format!("`{}` is already included", alias.text), alias.span);
                            continue;
                        }
                        match imports.kind(&include.source.text, &included.name.text) {
                            Ok(kind) => {
                                self.definitions.insert(alias.text.as_str().into(), kind);
                            }
                            Err(message) => self.error(message, included.name.span),
                        }
                        module.includes.push(Include {
                            name: included.name.text.as_str().into(),
                            alias: alias.text.as_str().into(),
                            source: include.source.text.as_str().into(),
                            span: include.span,
                        });
                    }
                    continue;
                }
                _ => continue,
            };
            if self
                .definitions
                .insert(name.text.as_str().into(), kind)
                .is_some()
            {
                self.error(format!("`{}` is already declared", name.text), name.span);
            }
        }
    }

    /// A value evaluated on its own, as a function without parameters.
    fn expression_code(&mut self, name: &str, value: &ast::Expression, span: Span) -> Code {
        self.scopes.push_frame(false);
        let value = self.expression(value);
        let layout = self.scopes.pop_frame();
        Code {
            name: name.into(),
            parameters: Vec::new(),
            implicit_parameter: false,
            slot_count: layout.slot_count,
            captures: layout.captures,
            body: vec![Statement {
                span: value.span,
                kind: StatementKind::Expression(value),
            }],
            span,
        }
    }

    /// The entry workflow of a script that has only statements.
    fn implicit_workflow(&mut self, statements: &[&ast::Statement]) -> Workflow {
        self.scopes.push_frame(false);
        self.in_workflow = true;
        self.implicit_declarations = true;
        let statements: Vec<ast::Statement> = statements.iter().map(|&s| s.clone()).collect();
        let main = self.statements(&statements);
        self.implicit_declarations = false;
        self.in_workflow = false;
        let layout = self.scopes.pop_frame();
        let span = match (statements.first(), statements.last()) {
            (Some(first), Some(last)) => first.span.to(last.span),
            _ => Span::default(),
        };
        Workflow {
            name: None,
            slot_count: layout.slot_count,
            takes: Vec::new(),
            main,
            emits: Vec::new(),
            span,
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
