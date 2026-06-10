//! Lowering: resolves the names of a syntax tree as Nextflow's compiler does, and reports the
//! same load-time errors, like "`x` is not defined".
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod class;
mod criteria;
mod dsl1;
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
    Code, DefinitionKind, Include, Module, Parameter, Statement, StatementKind, TypedParameter,
    Workflow,
};
use crate::value::Value;
use scope::{Earlier, Scopes};

/// Tells lowering what a name that a script includes stands for.
pub trait Imports {
    /// The kind of `name` in the script that `source` names, relative to the including script,
    /// or the error to report.
    fn kind(&mut self, source: &str, name: &str) -> Result<DefinitionKind, String>;

    /// The names of the Groovy classes of the pipeline's `lib/` directory, which every script
    /// can use. The default has none.
    fn classes(&mut self) -> Vec<Rc<str>> {
        Vec::new()
    }
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
    lower_legacy_config_value(value, source, process_scope, &ConfigNames::default())
}

/// The names that the legacy parser adds to config code (decision 14). It runs a config file
/// as a Groovy script, so the code may call the functions of the file and of the files that
/// include it, and read local variables and earlier settings by name.
#[derive(Default)]
pub struct ConfigNames<'a> {
    /// The functions of the file.
    pub functions: HashSet<Rc<str>>,
    /// The functions of the files that include this one, which the module links.
    pub inherited: HashSet<Rc<str>>,
    /// The value of another name that strict syntax does not know, given whether the code
    /// reads it inside a closure. The value is fixed when the code is lowered.
    pub lookup: Option<&'a Lookup<'a>>,
}

/// Gives the value of a name, given whether the code reads it inside a closure.
pub type Lookup<'a> = dyn Fn(&str, bool) -> Option<Value> + 'a;

/// Lowers the value of one config setting like `lower_config_value`, with the names of
/// legacy syntax.
pub fn lower_legacy_config_value(
    value: &ast::Expression,
    source: &str,
    process_scope: bool,
    names: &ConfigNames,
) -> Result<Code, Vec<Diagnostic>> {
    let mut lowering = Lowering::config(source, names);
    lowering.dynamic_names = process_scope;
    let code = lowering.expression_code("config", value, value.span);
    lowering.finish(code)
}

/// Lowers a statement of legacy config syntax, like `System.err.println('...')`, as code
/// without parameters.
pub fn lower_config_statement(
    statement: &ast::Statement,
    source: &str,
    names: &ConfigNames,
) -> Result<Code, Vec<Diagnostic>> {
    let mut lowering = Lowering::config(source, names);
    lowering.scopes.push_frame(false);
    let body = lowering.statements(std::slice::from_ref(statement));
    let layout = lowering.scopes.pop_frame();
    let code = Code {
        name: "config".into(),
        parameters: Vec::new(),
        implicit_parameter: false,
        slot_count: layout.slot_count,
        captures: layout.captures,
        body,
        span: statement.span,
    };
    lowering.finish(code)
}

/// Lowers a function of legacy config syntax, like `def check_max(obj, type) { ... }`. Like a
/// Groovy method, it cannot read the local variables of the file.
pub fn lower_config_function(
    function: &ast::Function,
    source: &str,
    names: &ConfigNames,
) -> Result<Code, Vec<Diagnostic>> {
    let names = ConfigNames {
        functions: names.functions.clone(),
        inherited: names.inherited.clone(),
        lookup: None,
    };
    let mut lowering = Lowering::config(source, &names);
    let code = lowering.function(function);
    lowering.finish(code)
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
    // A DSL1 script runs as a DSL2 script whose entry workflow calls its processes.
    let converted;
    let mut dsl1_names = Vec::new();
    let script = if dsl1::is_dsl1(script) {
        let (script, channels) = dsl1::convert(script);
        converted = script;
        for item in &converted.items {
            if let Item::Workflow(workflow) = item {
                for statement in &workflow.main.statements {
                    script_variables(statement, true, &mut dsl1_names);
                }
            }
        }
        dsl1_names.extend(channels);
        &converted
    } else {
        script
    };
    let mut lowering = Lowering::new(source);
    let mut module = Module::default();
    lowering.declare_definitions(script, imports, &mut module);
    lowering.declare_classes(script, imports);
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
    // Legacy syntax: statements next to declarations run when the script loads, and the
    // variables they declare or assign belong to the whole script.
    let legacy = declares && !statements.is_empty();
    if legacy {
        let mut names = Vec::new();
        for statement in &statements {
            script_variables(statement, true, &mut names);
        }
        for name in names {
            lowering.scopes.declare_module(&name);
        }
    }
    // The variables and channels of a DSL1 script belong to the script, as Groovy's binding
    // keeps them, so that its processes and functions read them too.
    for name in &dsl1_names {
        lowering.scopes.declare_module(name);
    }
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
            // In a script with statements, they run with the statements, in order.
            Item::LegacyParameter { .. } if legacy => {}
            Item::LegacyParameter { path, value, span } => {
                let path = path.iter().map(|name| name.text.as_str().into()).collect();
                let code = lowering.expression_code("params", value, *span);
                module.parameters.push((path, Rc::new(code)));
            }
            Item::Parameters { parameters, .. } => {
                for parameter in parameters {
                    match parameter {
                        ast::ParameterDeclaration::Parameter {
                            name,
                            annotation,
                            default,
                        } => {
                            let default = default.as_ref().map(|value| {
                                Rc::new(lowering.expression_code("params", value, name.span))
                            });
                            module.typed_parameters.push(TypedParameter {
                                name: name.text.as_str().into(),
                                class: annotation.as_ref().and_then(|annotation| {
                                    annotation
                                        .name
                                        .parts
                                        .last()
                                        .map(|part| part.text.as_str().into())
                                }),
                                nullable: annotation.as_ref().is_some_and(|a| a.nullable),
                                default,
                                span: name.span,
                            });
                        }
                        ast::ParameterDeclaration::Statement(statement) => {
                            lowering.error("Invalid parameter declaration", statement.span);
                        }
                    }
                }
            }
            // The directives of the outputs are not read yet: iunctus does not publish files.
            Item::Output { targets, .. } => {
                for target in targets {
                    match target {
                        ast::OutputTarget::Target { name, .. } => {
                            module.output_names.push(name.text.as_str().into());
                        }
                        ast::OutputTarget::Statement(statement) => {
                            lowering.error("Invalid output declaration", statement.span);
                        }
                    }
                }
            }
            Item::Class(class) => lowering.class(class, None, &mut module.classes),
            // Legacy syntax: an import names a class; only static members matter here.
            Item::Include(_)
            | Item::FeatureFlag { .. }
            | Item::Statement(_)
            | Item::Import { .. } => {}
            other => lowering.error("This script item is not supported yet", item_span(other)),
        }
    }
    if !declares && !statements.is_empty() {
        let workflow = lowering.implicit_workflow(&statements);
        module.workflows.push(Rc::new(workflow));
    }
    if legacy {
        let items: Vec<&Item> = script
            .items
            .iter()
            .filter(|item| matches!(item, Item::Statement(_) | Item::LegacyParameter { .. }))
            .collect();
        module.initializer = Some(Rc::new(lowering.initializer(&items)));
    }
    module.variables = (0..lowering.scopes.module_count())
        .map(|_| Rc::new(std::cell::RefCell::new(Value::Null)))
        .collect();
    module.legacy = std::mem::take(&mut lowering.legacy);
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
    /// Whether the code is the body of a function, which cannot set a constant of the script.
    in_function: bool,
    /// Whether assigning an undeclared name declares it, as in process and workflow sections.
    implicit_declarations: bool,
    /// Whether the code is a config value, which knows fewer names than a script.
    config: bool,
    /// Whether closures resolve unknown names when a task runs, as in the config `process` scope.
    dynamic_names: bool,
    /// Whether the code is a template expression, where every unknown name is resolved late.
    template: bool,
    /// The value of a name of legacy config syntax, from `ConfigNames::lookup`.
    lookup: Option<&'a Lookup<'a>>,
    /// Legacy syntax that strict syntax rejects with these errors, for `Module::legacy`.
    legacy: Vec<Diagnostic>,
    /// The Groovy classes that code can name: those of the `lib/` directory and of the file.
    library: HashSet<Rc<str>>,
    /// The class being lowered, whose members its code can name without qualification.
    class: Option<class::ClassContext>,
    /// Static members that `import static` brings in, with their class.
    static_imports: HashMap<Rc<str>, Rc<str>>,
    /// The classes of `IMPORTED_CLASSES` that the script imports.
    imported_classes: HashSet<Rc<str>>,
    /// Legacy syntax: the indexes in `Module::includes` of each include inside an `if` of the
    /// script, by its position.
    conditional_includes: HashMap<Span, Vec<usize>>,
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
            in_function: false,
            implicit_declarations: false,
            config: false,
            dynamic_names: false,
            template: false,
            lookup: None,
            legacy: Vec::new(),
            library: HashSet::new(),
            class: None,
            static_imports: HashMap::new(),
            imported_classes: HashSet::new(),
            conditional_includes: HashMap::new(),
        }
    }

    /// For config code, with the names of legacy syntax.
    fn config(source: &'a str, names: &ConfigNames<'a>) -> Self {
        let mut lowering = Lowering::new(source);
        lowering.config = true;
        lowering.functions = names.functions.clone();
        for name in &names.inherited {
            lowering
                .definitions
                .insert(name.clone(), DefinitionKind::Function);
        }
        lowering.lookup = names.lookup;
        lowering
    }

    /// The code, or the errors found while lowering it.
    fn finish(self, code: Code) -> Result<Code, Vec<Diagnostic>> {
        if self.errors.is_empty() {
            Ok(code)
        } else {
            Err(self.errors)
        }
    }

    fn error(&mut self, message: impl Into<String>, span: Span) {
        self.errors.push(Diagnostic::new(message, span));
    }

    /// Records legacy syntax, which iunctus accepts with the meaning of the legacy parser
    /// (decision 14), with the error of strict syntax.
    fn legacy(&mut self, message: impl Into<String>, span: Span) {
        self.legacy.push(Diagnostic::new(message, span));
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
        self.in_function = true;
        let body = self.statements(&function.body.statements);
        self.in_function = false;
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

    /// Records the Groovy classes that the script can name and its static imports, before
    /// lowering any code.
    fn declare_classes(&mut self, script: &ast::Script, imports: &mut dyn Imports) {
        self.library.extend(imports.classes());
        for item in &script.items {
            match item {
                Item::Class(class) => {
                    self.library.insert(class.name.text.as_str().into());
                }
                Item::Import {
                    name,
                    member: false,
                    ..
                } => {
                    if let Some(class) = name.parts.last()
                        && names::IMPORTED_CLASSES.contains(&class.text.as_str())
                    {
                        self.imported_classes.insert(class.text.as_str().into());
                    }
                }
                Item::Import {
                    name,
                    alias,
                    member: true,
                    ..
                } => {
                    let [.., class, member] = name.parts.as_slice() else {
                        continue;
                    };
                    let local = alias.as_ref().unwrap_or(member);
                    self.static_imports
                        .insert(local.text.as_str().into(), class.text.as_str().into());
                }
                _ => {}
            }
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
                    self.declare_include(include, false, imports, module);
                    continue;
                }
                Item::Statement(statement) => {
                    self.declare_conditional_includes(statement, imports, module);
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
            bare_emits: Vec::new(),
            publishers: Vec::new(),
            span,
        }
    }

    /// The statements of a script that also has declarations, as code without parameters. A
    /// `def` at the top declares a variable of the script.
    fn initializer(&mut self, items: &[&Item]) -> Code {
        self.scopes.push_frame(false);
        let mut body = Vec::new();
        for item in items {
            match item {
                Item::Statement(statement) => {
                    body.extend(self.statement(&script_assignment(statement)));
                }
                Item::LegacyParameter { path, value, span } => body.push(Statement {
                    kind: StatementKind::SetParam {
                        path: path.iter().map(|name| name.text.as_str().into()).collect(),
                        value: self.expression(value),
                    },
                    span: *span,
                }),
                _ => {}
            }
        }
        let layout = self.scopes.pop_frame();
        let span = match (items.first(), items.last()) {
            (Some(first), Some(last)) => item_span(first).to(item_span(last)),
            _ => Span::default(),
        };
        Code {
            name: "script".into(),
            parameters: Vec::new(),
            implicit_parameter: false,
            slot_count: layout.slot_count,
            captures: layout.captures,
            body,
            span,
        }
    }

    /// Declares the names of an include. A conditional include may give a name that another
    /// conditional include of the same kind gives too, as the branches of an `if` do.
    fn declare_include(
        &mut self,
        include: &ast::Include,
        conditional: bool,
        imports: &mut dyn Imports,
        module: &mut Module,
    ) -> Vec<usize> {
        let mut indexes = Vec::new();
        for included in &include.names {
            let alias = included.alias.as_ref().unwrap_or(&included.name);
            let kind = match imports.kind(&include.source.text, &included.name.text) {
                Ok(kind) => kind,
                Err(message) => {
                    self.error(message, included.name.span);
                    continue;
                }
            };
            let earlier = self.definitions.get(alias.text.as_str()).copied();
            let again = module
                .includes
                .iter()
                .any(|earlier| earlier.conditional && conditional && *earlier.alias == *alias.text);
            if earlier.is_some_and(|earlier| !again || earlier != kind) {
                self.error(format!("`{}` is already included", alias.text), alias.span);
                continue;
            }
            self.definitions.insert(alias.text.as_str().into(), kind);
            indexes.push(module.includes.len());
            module.includes.push(Include {
                name: included.name.text.as_str().into(),
                alias: alias.text.as_str().into(),
                source: include.source.text.as_str().into(),
                conditional,
                span: include.span,
            });
        }
        indexes
    }

    /// Legacy syntax: declares the includes inside the `if` statements of the script.
    fn declare_conditional_includes(
        &mut self,
        statement: &ast::Statement,
        imports: &mut dyn Imports,
        module: &mut Module,
    ) {
        match &statement.kind {
            ast::StatementKind::Include(include) => {
                let indexes = self.declare_include(include, true, imports, module);
                self.conditional_includes.insert(include.span, indexes);
            }
            ast::StatementKind::If {
                then, otherwise, ..
            } => {
                let blocks = std::iter::once(then).chain(otherwise);
                for statement in blocks.flat_map(|block| &block.statements) {
                    self.declare_conditional_includes(statement, imports, module);
                }
            }
            _ => {}
        }
    }

    /// Declares a variable in the current block, reporting a name declared before. Legacy
    /// syntax may declare again a name that a binding holds.
    fn declare(&mut self, name: &ast::Name) -> usize {
        self.declare_as(name, false)
    }

    /// Declares a variable; `binding` marks a name that the legacy parser keeps in a binding,
    /// like a process input or a take, which legacy code may hide with a local of the same name.
    fn declare_as(&mut self, name: &ast::Name, binding: bool) -> usize {
        let (slot, earlier) = self.scopes.declare(&name.text, binding);
        let message = format!("`{}` is already declared", name.text);
        match earlier {
            Some(Earlier::Local) => self.error(message, name.span),
            // Legacy syntax: the name in the binding stays for the code that the local does
            // not reach, like a process output.
            Some(Earlier::Binding) => self.legacy(message, name.span),
            None => {}
        }
        slot
    }
}

/// The variables of the script that a top-level statement declares with `def`, or assigns
/// outside closures, as Groovy's script binding keeps them. A top-level `ch.set { x }` also
/// assigns `x`, as Nextflow's `set` sets the variable in the binding.
fn script_variables(statement: &ast::Statement, top: bool, names: &mut Vec<String>) {
    use ast::StatementKind as Kind;
    let mut add = |name: &str| {
        let constant =
            names::SCRIPT_CONSTANTS.contains(&name) || names::ENTRY_CONSTANTS.contains(&name);
        if !constant {
            names.push(name.to_owned());
        }
    };
    match &statement.kind {
        Kind::Declaration {
            names: declared, ..
        } if top => {
            for typed in declared {
                add(&typed.name.text);
            }
        }
        Kind::Assign { target, .. } => {
            if let ast::ExpressionKind::Variable(name) = &target.kind {
                add(name);
            }
        }
        Kind::MultipleAssign {
            names: assigned, ..
        } => {
            for name in assigned {
                add(&name.text);
            }
        }
        Kind::If {
            then, otherwise, ..
        } => {
            let blocks = std::iter::once(then).chain(otherwise);
            for statement in blocks.flat_map(|block| &block.statements) {
                script_variables(statement, false, names);
            }
        }
        Kind::Try { body, catches } => {
            let blocks = std::iter::once(body).chain(catches.iter().map(|catch| &catch.body));
            for statement in blocks.flat_map(|block| &block.statements) {
                script_variables(statement, false, names);
            }
        }
        Kind::Switch { cases, .. } => {
            for statement in cases.iter().flat_map(|case| &case.body) {
                script_variables(statement, false, names);
            }
        }
        Kind::Labeled { statement, .. } => script_variables(statement, top, names),
        Kind::Expression(_) if top => {
            if let Some((_, name, _)) = workflow::set_parts(statement) {
                add(name);
            }
        }
        _ => {}
    }
}

/// A top-level `def` as an assignment of the script variable it declares, and a top-level
/// `ch.set { x }` as the assignment `x = ch`.
pub(super) fn script_assignment(statement: &ast::Statement) -> ast::Statement {
    use ast::StatementKind as Kind;
    if let Some((source, name, span)) = workflow::set_parts(statement) {
        let kind = Kind::Assign {
            target: ast::Expression {
                kind: ast::ExpressionKind::Variable(name.to_owned()),
                span,
            },
            operator: ast::AssignOperator::Assign,
            value: source.clone(),
        };
        return ast::Statement {
            kind,
            span: statement.span,
        };
    }
    let Kind::Declaration {
        names,
        initializer,
        destructure,
        ..
    } = &statement.kind
    else {
        return statement.clone();
    };
    let value = initializer.clone().unwrap_or(ast::Expression {
        kind: ast::ExpressionKind::Literal(ast::Literal::Null),
        span: statement.span,
    });
    let kind = match (destructure, names.as_slice()) {
        (false, [typed]) => Kind::Assign {
            target: ast::Expression {
                kind: ast::ExpressionKind::Variable(typed.name.text.clone()),
                span: typed.name.span,
            },
            operator: ast::AssignOperator::Assign,
            value,
        },
        _ => Kind::MultipleAssign {
            names: names.iter().map(|typed| typed.name.clone()).collect(),
            value,
        },
    };
    ast::Statement {
        kind,
        span: statement.span,
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
        Item::Class(class) => class.span,
        Item::Statement(statement) => statement.span,
    }
}
