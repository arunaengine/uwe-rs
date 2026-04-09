//! Evaluation of Snakefiles: the Python code runs in the interpreter, and each Snakemake
//! statement runs when the code reaches it, as the Python that Snakemake generates would.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use indexmap::IndexMap;
use python_evaluator::ruff_python_ast as ast;
use python_evaluator::ruff_text_size::{Ranged, TextSize};
use python_evaluator::{
    Args, Dict, Error, Frame, Interpreter, Scope, Source, StatementHook, Value,
};
use snakemake_parser::ast::{
    Directive, Module, Snakefile, Statement, UseRule, Value as DirectiveValue,
};
use snakemake_parser::keyword::{GlobalKeyword, ModuleKeyword, RuleKeyword};

use crate::functions;
use crate::rule::{Action, Items, Rule, retag};

/// A `WorkflowError`, the error Snakemake raises for mistakes in a workflow.
pub fn workflow_error(message: impl Into<String>) -> Error {
    let class = python_evaluator::types::library_exception(
        "WorkflowError",
        "snakemake.exceptions",
        "Exception",
    );
    Error::from_value(python_evaluator::error::exception(
        &class,
        vec![Value::str(message.into())],
    ))
}

/// Files with the names that `multiext` gives them.
type NamedFiles = Vec<(Value, Option<String>)>;

/// The evaluated directive values of a rule, which `use rule` copies and overrides.
#[derive(Clone, Default)]
pub struct RuleInfo {
    pub name: Option<String>,
    pub checkpoint: bool,
    pub directives: Vec<(RuleKeyword, Args)>,
    pub run: Option<String>,
    pub docstring: Option<String>,
}

impl RuleInfo {
    /// Replaces directives with those of a `with:` block; keyword params are merged.
    fn overwrite(&mut self, with: &RuleInfo) {
        for (keyword, args) in &with.directives {
            match self
                .directives
                .iter_mut()
                .find(|(existing, _)| existing == keyword)
            {
                Some((_, existing)) if *keyword == RuleKeyword::Parameters => {
                    if !args.positional.is_empty() {
                        existing.positional = args.positional.clone();
                    }
                    for (name, value) in &args.keywords {
                        existing.keywords.retain(|(key, _)| key != name);
                        existing.keywords.push((name.clone(), value.clone()));
                    }
                }
                Some((_, existing)) => *existing = args.clone(),
                None => self.directives.push((*keyword, args.clone())),
            }
        }
        if with.run.is_some() {
            self.run = with.run.clone();
        }
    }

    fn has(&self, keyword: RuleKeyword) -> bool {
        self.directives
            .iter()
            .any(|(existing, _)| *existing == keyword)
    }
}

/// A `module` statement.
#[derive(Clone)]
pub struct ModuleSpec {
    pub name: String,
    pub snakefile: Option<Value>,
    pub meta_wrapper: Option<Value>,
    pub config: Option<Value>,
    pub prefix: Option<String>,
    pub replace_prefix: Vec<(String, String)>,
    pub skip_validation: bool,
    pub basedir: PathBuf,
}

/// The context of the code that runs: the main workflow, or a module that `use rule`
/// imports rules from, as Snakemake's `WorkflowModifier`.
pub struct Modifier {
    pub globals: Rc<Scope>,
    /// Rule names as this code knows them, and as the workflow registers them.
    pub rule_names: Rc<RefCell<IndexMap<String, String>>>,
    pub name_modifier: Option<String>,
    pub whitelist: Option<Vec<String>>,
    pub exclude: Vec<String>,
    pub prefix: Option<String>,
    pub replace_prefix: Vec<(String, String)>,
    pub overwrite: Option<RuleInfo>,
    pub skip_configfile: bool,
    pub skip_validation: bool,
    pub wildcard_constraints: IndexMap<String, String>,
    pub container: Option<Value>,
    pub containerized: bool,
    pub is_module: bool,
    pub modules: IndexMap<String, ModuleSpec>,
    pub allow_overwrite: bool,
}

impl Modifier {
    fn root(globals: Rc<Scope>) -> Modifier {
        Modifier {
            globals,
            rule_names: Rc::new(RefCell::new(IndexMap::new())),
            name_modifier: None,
            whitelist: None,
            exclude: Vec::new(),
            prefix: None,
            replace_prefix: Vec::new(),
            overwrite: None,
            skip_configfile: false,
            skip_validation: false,
            wildcard_constraints: IndexMap::new(),
            container: None,
            containerized: false,
            is_module: false,
            modules: IndexMap::new(),
            allow_overwrite: false,
        }
    }
}

pub struct State {
    pub rules: Vec<Rule>,
    pub infos: Vec<RuleInfo>,
    pub ruleorder: Vec<Vec<String>>,
    pub localrules: Vec<String>,
    pub default_target: Option<String>,
    /// Wildcard constraints of the main workflow.
    pub global_constraints: IndexMap<String, String>,
    pub global_conda: Option<Value>,
    pub config: Value,
    pub overwrite_config: Value,
    pub overwrite_configfiles: Vec<String>,
    pub configfiles: Vec<String>,
    pub included: Vec<PathBuf>,
    pub include_stack: Vec<PathBuf>,
    pub main_snakefile: PathBuf,
    pub workdir: PathBuf,
    pub globals: Rc<Scope>,
    pub scatter: IndexMap<String, i64>,
    pub envvars: Vec<String>,
    pub cores: i64,
    pub shell_prefix: Option<String>,
    pub shell_suffix: String,
    pub shell_executable: Option<String>,
    pub modifiers: Vec<Modifier>,
    /// Parsed Snakefiles by path, for the statements their code reaches.
    pub snakefiles: HashMap<Rc<str>, Rc<Snakefile>>,
}

impl State {
    pub fn new(globals: Rc<Scope>, main_snakefile: PathBuf, workdir: PathBuf) -> State {
        State {
            rules: Vec::new(),
            infos: Vec::new(),
            ruleorder: Vec::new(),
            localrules: Vec::new(),
            default_target: None,
            global_constraints: IndexMap::new(),
            global_conda: None,
            config: Value::dict(Dict::default()),
            overwrite_config: Value::dict(Dict::default()),
            overwrite_configfiles: Vec::new(),
            configfiles: Vec::new(),
            included: Vec::new(),
            include_stack: Vec::new(),
            main_snakefile,
            workdir,
            globals: globals.clone(),
            scatter: IndexMap::new(),
            envvars: Vec::new(),
            cores: std::thread::available_parallelism().map_or(1, |count| count.get()) as i64,
            shell_prefix: None,
            shell_suffix: String::new(),
            shell_executable: None,
            modifiers: vec![Modifier::root(globals)],
            snakefiles: HashMap::new(),
        }
    }

    pub fn rule(&self, name: &str) -> Option<&Rule> {
        self.rules.iter().find(|rule| rule.name == name)
    }

    pub fn rule_index(&self, name: &str) -> Option<usize> {
        self.rules.iter().position(|rule| rule.name == name)
    }

    /// The folder of the Snakefile that runs now.
    pub fn current_basedir(&self) -> PathBuf {
        let file = self.include_stack.last().unwrap_or(&self.main_snakefile);
        file.parent().map(Path::to_path_buf).unwrap_or_default()
    }

    fn modifier(&self) -> &Modifier {
        self.modifiers.last().expect("the root modifier")
    }

    /// The name a rule gets in the workflow, or `None` when the current `use rule` skips it.
    fn available_name(&self, name: &str) -> Option<String> {
        let mut current = name.to_owned();
        for modifier in self.modifiers.iter().rev() {
            if let Some(whitelist) = &modifier.whitelist
                && !whitelist.contains(&current)
            {
                return None;
            }
            if modifier.exclude.contains(&current) {
                return None;
            }
            if let Some(pattern) = &modifier.name_modifier {
                current = pattern.replace('*', &current);
            }
        }
        Some(current)
    }

    /// A path with the prefixes of the modules that load the current code.
    fn modify_path(&self, value: &Value, property: &str, skips: &[String]) -> Value {
        if value.tag("path_modified").is_some() || value.tag("local").is_some() {
            return value.clone();
        }
        let Some(mut path) = value.as_str().map(str::to_owned) else {
            return value.clone();
        };
        let original = path.clone();
        for modifier in self.modifiers.iter().rev() {
            if !modifier.is_module
                || skips.iter().any(|skip| skip == property)
                || path.starts_with('/')
                || path.starts_with("..")
            {
                continue;
            }
            if let Some(prefix) = &modifier.prefix {
                let prefix = if prefix.ends_with('/') {
                    prefix.clone()
                } else {
                    format!("{prefix}/")
                };
                path = format!("{prefix}{path}");
            } else if let Some((from, to)) = modifier
                .replace_prefix
                .iter()
                .filter(|(from, _)| path.starts_with(from.as_str()))
                .max_by_key(|(from, _)| from.len())
            {
                path = format!("{to}{}", &path[from.len()..]);
            }
        }
        if path == original {
            return value.clone();
        }
        retag(value, Value::str(path)).with_tag("path_modified", Value::Bool(true))
    }
}

/// Runs the Snakemake statements that the Python code of a Snakefile reaches.
pub struct Hook {
    pub state: Rc<RefCell<State>>,
}

impl StatementHook for Hook {
    fn run(
        &self,
        interpreter: &mut Interpreter,
        frame: &Frame,
        offset: TextSize,
    ) -> Result<bool, Error> {
        let file = self
            .state
            .borrow()
            .snakefiles
            .get(&frame.source.path)
            .cloned();
        let Some(file) = file else {
            return Ok(false);
        };
        let Some(statement) = file.statement_at(offset) else {
            return Ok(false);
        };
        match statement {
            Statement::Global(directive) => self.global(interpreter, frame, directive)?,
            Statement::Rule(rule) => {
                let info = self.rule_info(
                    interpreter,
                    frame,
                    rule.checkpoint,
                    rule.name.as_ref().map(|name| name.text.clone()),
                    &rule.directives,
                    &rule.docstrings,
                )?;
                let line = frame.source.line(rule.range.start());
                self.define_rule(interpreter, info, line, &frame.source.path)?;
            }
            Statement::UseRule(rule) => self.use_rule(interpreter, frame, rule)?,
            Statement::Module(module) => self.module(interpreter, frame, module)?,
            Statement::Storage(_) => return Err(Error::unsupported("storage directives")),
        }
        Ok(true)
    }
}

impl Hook {
    /// Includes a Snakefile: parses it and runs its code with the workflow's globals.
    pub fn include(
        &self,
        interpreter: &mut Interpreter,
        path: &Path,
        overwrite_default_target: bool,
    ) -> Result<(), Error> {
        {
            let state = self.state.borrow();
            let in_module = state.modifier().is_module;
            if !in_module && state.included.iter().any(|included| included == path) {
                return Ok(());
            }
        }
        let text = interpreter
            .host
            .read(path)
            .map_err(|error| Error::os_error(&error, &path.to_string_lossy()))?;
        let text = String::from_utf8_lossy(&text).into_owned();
        let name = path.to_string_lossy().into_owned();
        let source = Source::new(&name, &text);
        let snakefile = snakemake_parser::parse(&text).map_err(|error| {
            Error::new("SyntaxError", error.message.clone())
                .with_location(source.location(error.range.start()))
        })?;
        let snakefile = Rc::new(snakefile);
        let (globals, saved_target) = {
            let mut state = self.state.borrow_mut();
            state
                .snakefiles
                .insert(source.path.clone(), snakefile.clone());
            state.included.push(path.to_owned());
            state.include_stack.push(path.to_owned());
            (
                state.modifier().globals.clone(),
                state.default_target.clone(),
            )
        };
        if let Some(folder) = path.parent() {
            let folder = Value::str(folder.to_string_lossy().as_ref());
            if !interpreter.contains(&interpreter.sys_path.clone(), &folder)?
                && let Value::List(items) = interpreter.sys_path.plain()
            {
                items.borrow_mut().insert(0, folder);
            }
        }
        let frame = Frame::module(globals, source);
        let result = interpreter.run(&snakefile.python, &frame);
        let mut state = self.state.borrow_mut();
        state.include_stack.pop();
        if !overwrite_default_target {
            state.default_target = saved_target;
        }
        result
    }
}
