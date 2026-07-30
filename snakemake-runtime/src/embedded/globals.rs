//! The Snakefile's globals that the code of a `run:` block uses, carried over from the
//! native interpreter as Python code: plain values as literals, tables as Snakemake stores
//! them, modules as imports (local ones with their files), functions of the workflow by their
//! source text after what they use, and objects that only the interpreter has as objects that
//! raise an error when the block uses them.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use python_evaluator::format::quote;
use python_evaluator::ruff_python_ast::visitor::{self, Visitor};
use python_evaluator::ruff_python_ast::{Expr, Stmt};
use python_evaluator::ruff_text_size::Ranged;
use python_evaluator::value::{Code, Function};
use python_evaluator::{Interpreter, Scope, Value};
use workflow_graph::Input;

use super::data::{self, Data};
use super::python;

/// The names that expressions in Python code use, once each, in the order of the syntax tree.
pub fn used_names(text: &str) -> Vec<String> {
    struct Names(Vec<String>);
    impl<'a> Visitor<'a> for Names {
        fn visit_expr(&mut self, expr: &'a Expr) {
            if let Expr::Name(name) = expr
                && !self.0.iter().any(|known| known == name.id.as_str())
            {
                self.0.push(name.id.to_string());
            }
            visitor::walk_expr(self, expr);
        }

        fn visit_stmt(&mut self, stmt: &'a Stmt) {
            visitor::walk_stmt(self, stmt);
        }
    }
    let Ok(suite) = Interpreter::parse("<run>", text) else {
        return Vec::new();
    };
    let mut names = Names(Vec::new());
    for statement in &suite {
        names.visit_stmt(statement);
    }
    names.0
}

/// An import that binds `name`, or an object that says which module the image lacks.
fn guarded(statement: &str, name: &str, module: &str) -> String {
    format!(
        "try:\n    {statement}\nexcept ImportError as error:\n    {name} = __iunctus.Unavailable({} + str(error))\n",
        quote(&format!(
            "the run: block uses the module {module}, which the image of the task cannot import: "
        ))
    )
}

/// An object that raises, when the block uses it, why the value could not go.
fn unavailable(name: &str, repr: &str) -> String {
    format!(
        "{name} = __iunctus.Unavailable({})\n",
        quote(&format!(
            "{name} is {repr}, an object that only the native interpreter of iunctus has, so \
             the run: block cannot use it in the container of its task"
        ))
    )
}

/// The Python text of a function or lambda of the workflow, dedented.
fn source_of(function: &Function) -> String {
    let text = &function.source.text;
    let (start, end) = match &function.code {
        Code::Def(def) => {
            let start = def
                .decorator_list
                .first()
                .map_or(def.range().start(), |decorator| {
                    decorator.range().start().min(def.range().start())
                });
            (start.to_usize(), def.range().end().to_usize())
        }
        Code::Lambda(lambda) => {
            return text[lambda.range().start().to_usize()..lambda.range().end().to_usize()]
                .to_owned();
        }
    };
    let line_start = text[..start].rfind('\n').map_or(0, |index| index + 1);
    let from = if text[line_start..start].trim().is_empty() {
        line_start
    } else {
        start
    };
    python_evaluator::modules::dedent(&text[from..end])
}

/// The globals of the Snakefile that code uses, carried over as Python code.
pub struct Transfer<'a> {
    interpreter: &'a mut Interpreter,
    globals: Rc<Scope>,
    workdir: &'a Path,
    /// Names that are not carried over: the parameters of the rule function, and `config`,
    /// which the module gets from the job.
    skipped: HashSet<&'static str>,
    /// Names that the module imports, unless the workflow bound them to its own values.
    imported: HashSet<&'static str>,
    seen: HashSet<String>,
    /// The definitions, each after what it uses.
    pub code: Vec<String>,
    /// Files of local modules, which the task sees at their paths.
    pub inputs: Vec<Input>,
    /// The folders that local modules are imported from.
    pub searchpaths: Vec<String>,
}

impl<'a> Transfer<'a> {
    pub fn new(
        interpreter: &'a mut Interpreter,
        globals: Rc<Scope>,
        workdir: &'a Path,
        skipped: HashSet<&'static str>,
        imported: HashSet<&'static str>,
    ) -> Transfer<'a> {
        Transfer {
            interpreter,
            globals,
            workdir,
            skipped,
            imported,
            seen: HashSet::new(),
            code: Vec::new(),
            inputs: Vec::new(),
            searchpaths: Vec::new(),
        }
    }

    /// Carries over the globals that some code uses.
    pub fn code_uses(&mut self, text: &str) {
        for name in used_names(text) {
            self.name(&name);
        }
    }

    fn name(&mut self, name: &str) {
        if self.skipped.contains(name) || !self.seen.insert(name.to_owned()) {
            return;
        }
        let Some(value) = self.globals.get(name) else {
            return;
        };
        // The module imports what every Snakefile sees, unless the workflow bound the name
        // to its own function or data.
        let own = matches!(value.plain(), Value::Function(_))
            || !matches!(
                value.plain(),
                Value::Module(_)
                    | Value::Builtin(_)
                    | Value::Class(_)
                    | Value::Method(_)
                    | Value::Native(_)
            );
        if self.imported.contains(name) && !own {
            return;
        }
        let code = self.value(name, &value);
        self.code.push(code);
    }

    fn value(&mut self, name: &str, value: &Value) -> String {
        match value.plain() {
            Value::Module(module) => {
                let file = module
                    .globals
                    .get("__file__")
                    .and_then(|file| file.as_str().map(PathBuf::from));
                if let Some(file) = file.filter(|file| file.is_file()) {
                    self.stage_module(&module.name, &file);
                }
                guarded(
                    &format!("import {} as {name}", module.name),
                    name,
                    &module.name,
                )
            }
            Value::Function(function) => self.function(name, function),
            Value::Builtin(_) | Value::Class(_) => match self.find_in_modules(value) {
                Some((module, attribute)) => guarded(
                    &format!("from {module} import {attribute} as {name}"),
                    name,
                    &module,
                ),
                None => {
                    let repr = self.repr(value);
                    unavailable(name, &repr)
                }
            },
            _ => {
                let repr = self.repr(value);
                // A module of the Python container of decision 19 shows as one.
                if let Some(module) = repr
                    .strip_prefix("<module '")
                    .and_then(|rest| rest.split_once('\''))
                    .map(|(module, _)| module.to_owned())
                {
                    return guarded(&format!("import {module} as {name}"), name, &module);
                }
                match data::from_value(self.interpreter, value) {
                    Data::Opaque { repr, .. } => unavailable(name, &repr),
                    Data::Table { kind, stored, .. } => {
                        format!("{name} = {}\n", python::table(kind, &stored))
                    }
                    data => format!("{name} = {}\n", python::literal(&data)),
                }
            }
        }
    }

    fn repr(&mut self, value: &Value) -> String {
        self.interpreter
            .repr(value)
            .unwrap_or_else(|_| python_evaluator::format::plain_repr(value))
    }

    /// A function of the workflow by its source text, after what it uses; a function of
    /// another module by an import from there.
    fn function(&mut self, name: &str, function: &Rc<Function>) -> String {
        if !Rc::ptr_eq(&function.globals, &self.globals) {
            let module = function
                .globals
                .get("__name__")
                .and_then(|module| module.as_str().map(str::to_owned))
                .unwrap_or_default();
            let file = function
                .globals
                .get("__file__")
                .and_then(|file| file.as_str().map(PathBuf::from));
            if let Some(file) = file.filter(|file| file.is_file()) {
                self.stage_module(&module, &file);
                return guarded(
                    &format!("from {module} import {} as {name}", function.name),
                    name,
                    &module,
                );
            }
            let repr = self.repr(&Value::Function(function.clone()));
            return unavailable(name, &repr);
        }
        let text = source_of(function);
        self.code_uses(&text);
        match &function.code {
            Code::Lambda(_) => format!("{name} = ({text})\n"),
            Code::Def(def) if def.name.as_str() == name => format!("{text}\n"),
            Code::Def(def) => format!("{text}\n{name} = {}\n", def.name),
        }
    }

    /// The module and attribute where a builtin function or class of a library is.
    fn find_in_modules(&self, value: &Value) -> Option<(String, String)> {
        let mut modules: Vec<(&String, &Value)> = self.interpreter.modules.iter().collect();
        modules.sort_by(|a, b| a.0.cmp(b.0));
        for (module_name, module) in modules {
            let Value::Module(module) = module else {
                continue;
            };
            for (attribute, item) in module.globals.vars.borrow().iter() {
                if item.is(value) {
                    return Some((module_name.clone(), attribute.to_string()));
                }
            }
        }
        None
    }

    /// Stages the file of a local module, or its top package, at its path on this machine,
    /// and puts the folder it is imported from on the module path.
    fn stage_module(&mut self, module: &str, file: &Path) {
        let parts = module.split('.').count();
        let package = file.file_name().is_some_and(|name| name == "__init__.py");
        let up = parts + usize::from(package);
        let Some(root) = file.ancestors().nth(up) else {
            return;
        };
        let top = if up > 1 {
            file.ancestors().nth(up - 1).unwrap_or(file)
        } else {
            file
        };
        // A package that holds the working directory would bring all its data along.
        let top = if top.is_dir() && self.workdir.starts_with(top) {
            file
        } else {
            top
        };
        let path = top.to_string_lossy().into_owned();
        if !self.inputs.iter().any(|input| input.path == path) {
            self.inputs.push(Input {
                path: path.clone(),
                source: path,
                directory: top.is_dir(),
                content: None,
                ..Default::default()
            });
        }
        let root = root.to_string_lossy().into_owned();
        if !self.searchpaths.contains(&root) {
            self.searchpaths.push(root);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::used_names;

    #[test]
    fn names_of_code_are_found_once() {
        let names = used_names("x = f(a)\nfor b in a:\n    g(b, x)\n");
        assert_eq!(names, ["f", "a", "x", "b", "g"]);
    }
}
