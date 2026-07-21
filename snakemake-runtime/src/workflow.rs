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
use crate::pathvars::{Level, Pathvars};
use crate::remote::Mirrors;
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

/// A file of a rule with its path variables replaced; other values stay as they are.
pub fn apply_pathvars(pathvars: &Pathvars, value: &Value) -> Result<Value, Error> {
    match value.as_str() {
        Some(text) => Ok(retag(value, Value::str(pathvars.apply(text)?))),
        None => Ok(value.clone()),
    }
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
    /// The `pathvars:` of the module statement.
    pub pathvars: Vec<(Value, Value)>,
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
    /// The path variables of the rules that this code defines.
    pub pathvars: Pathvars,
    /// The tag of the wrappers of an old meta-wrapper, which replaces `master/` in their names.
    pub wrapper_tag: Option<String>,
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
            pathvars: Pathvars::defaults(),
            wrapper_tag: None,
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
    /// The workflow profile, which Snakemake reads as default command-line options.
    pub profile: crate::profile::Profile,
    /// Local copies of workflow files on the web.
    pub mirrors: Rc<Mirrors>,
    /// Parsed Snakefiles by path, for the statements their code reaches.
    pub snakefiles: HashMap<Rc<str>, Rc<Snakefile>>,
    /// Rules of modules that `use rule` does not take. Other rules of the module may still
    /// read them as `rules.<name>`, so they are kept, but they are not part of the workflow.
    pub unused: Vec<Rule>,
    /// Resources that `workflow.register_resource` added.
    pub global_resources: IndexMap<String, Value>,
    /// The `validate` calls of the workflow code, for the schema of its inputs.
    pub validations: Vec<crate::inputs::Validation>,
    /// The PEP project of `pepfile:` and `pepschema:`.
    pub pep: Option<crate::inputs::Pep>,
}

impl State {
    pub fn new(
        globals: Rc<Scope>,
        main_snakefile: PathBuf,
        workdir: PathBuf,
        mirrors: Rc<Mirrors>,
    ) -> State {
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
            profile: crate::profile::Profile::default(),
            validations: Vec::new(),
            pep: None,
            mirrors,
            snakefiles: HashMap::new(),
            unused: Vec::new(),
            global_resources: IndexMap::new(),
        }
    }

    pub fn rule(&self, name: &str) -> Option<&Rule> {
        self.rules
            .iter()
            .chain(self.unused.iter())
            .find(|rule| rule.name == name)
    }

    pub fn rule_index(&self, name: &str) -> Option<usize> {
        self.rules.iter().position(|rule| rule.name == name)
    }

    /// The folder of the Snakefile that runs now.
    pub fn current_basedir(&self) -> PathBuf {
        let file = self.include_stack.last().unwrap_or(&self.main_snakefile);
        file.parent().map(Path::to_path_buf).unwrap_or_default()
    }

    pub fn modifier(&self) -> &Modifier {
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
            Statement::Storage(storage) => {
                let mut args = interpreter.eval_arguments(frame, &storage.arguments)?;
                let provider = match args.take_keyword("provider") {
                    Some(provider) if !provider.is_none() => Some(interpreter.to_str(&provider)?),
                    _ => None,
                };
                crate::storage::check_provider(provider.as_deref())?;
            }
        }
        Ok(true)
    }
}

impl Hook {
    /// Includes a Snakefile: parses it and runs its code with the workflow's globals.
    /// The Snakefile of a meta-wrapper of the Snakemake wrapper repository, like
    /// `v1.0.0/meta/bio/bwa_mapping`, and the tag that replaces `master/` in the names of its
    /// wrappers for meta-wrappers older than 8.0.0.
    fn meta_wrapper(
        &self,
        interpreter: &mut Interpreter,
        name: &str,
    ) -> Result<(PathBuf, Option<String>), Error> {
        let mirrors = self.state.borrow().mirrors.clone();
        let is_url = |text: &str| {
            text.starts_with("http") || text.starts_with("file:") || text.starts_with("git+file")
        };
        let version = |text: &str| -> Option<Vec<u64>> {
            let text = text.strip_prefix('v').unwrap_or(text);
            let parts: Vec<u64> = text
                .split('.')
                .take(3)
                .map(|part| {
                    let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
                    digits.parse().ok()
                })
                .collect::<Option<_>>()?;
            (parts.len() == 3).then_some(parts)
        };
        for file in ["/meta_wrapper.smk", "/test/Snakefile"] {
            let path = format!("{name}{file}");
            let local = if is_url(&path) {
                mirrors.url(&path).ok_or_else(|| {
                    Error::unsupported(format!("meta-wrappers at addresses like {path}"))
                })?
            } else {
                let (reference, tail) = path.split_once('/').unwrap_or((&path, ""));
                if version(reference).is_some() {
                    mirrors.hosted(
                        "github",
                        "github.com",
                        "snakemake/snakemake-wrappers",
                        reference,
                        tail,
                    )
                } else {
                    mirrors
                        .url(&format!(
                            "https://github.com/snakemake/snakemake-wrappers/raw/{path}"
                        ))
                        .unwrap_or_default()
                }
            };
            if interpreter.host.stat(&local).is_some() {
                let tag = if is_url(name) {
                    None
                } else {
                    let tag = name.split('/').next().unwrap_or_default();
                    match version(tag) {
                        Some(parts) if parts[0] >= 8 => None,
                        _ => Some(tag.to_owned()),
                    }
                };
                return Ok((local, tag));
            }
        }
        Err(workflow_error(format!(
            "Invalid meta wrapper {name}: Could not find meta_wrapper.smk or test/Snakefile (old style)."
        )))
    }

    /// The local path of a Snakefile that `include:` or a module names: a file in a git
    /// repository (`github(...)` or `gh:owner/repo@tag:path`), a web address, or a path
    /// relative to `base`. Files on the web are downloaded when they are read.
    fn source_path(
        &self,
        interpreter: &mut Interpreter,
        value: &Value,
        base: &Path,
    ) -> Result<PathBuf, Error> {
        if let Some(remote) = value.as_native::<functions::RemoteSource>() {
            if let Some(local) = &remote.local {
                return Ok(local.clone());
            }
            // A file of a local git repository at a reference, which Snakemake reads with git.
            let repository = remote.git_repository.clone().unwrap_or_default();
            let git = interpreter.host.resolve(&repository).join(".git");
            if interpreter.host.stat(&git).is_none() {
                return Err(workflow_error(format!(
                    "Failed to read source {} from git repo.",
                    remote.url
                )));
            }
            return Err(Error::unsupported(
                "Snakefiles in local git repositories (gitfile(...))",
            ));
        }
        let path = python_evaluator::modules::path_text(interpreter, value)?;
        let mirrors = self.state.borrow().mirrors.clone();
        if let Some(local) = mirrors.shorthand(&path) {
            return Ok(local);
        }
        if path.contains("://") {
            return mirrors
                .url(&path)
                .ok_or_else(|| Error::unsupported(format!("Snakefiles at addresses like {path}")));
        }
        let full = if Path::new(&path).is_absolute() {
            PathBuf::from(&path)
        } else {
            base.join(&path)
        };
        Ok(python_evaluator::host::normalize(&full))
    }

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

    fn global(
        &self,
        interpreter: &mut Interpreter,
        frame: &Frame,
        directive: &Directive<GlobalKeyword>,
    ) -> Result<(), Error> {
        let args = match &directive.value {
            DirectiveValue::Arguments(arguments) => interpreter.eval_arguments(frame, arguments)?,
            DirectiveValue::Names(names) => Args::new(
                names
                    .iter()
                    .map(|name| Value::str(name.text.as_str()))
                    .collect(),
            ),
            DirectiveValue::Expression(expression) => {
                Args::new(vec![interpreter.eval(frame, expression)?])
            }
            DirectiveValue::Body(_) => Args::default(),
        };
        let first = || args.positional.first().cloned().unwrap_or(Value::None);
        match directive.keyword {
            GlobalKeyword::Include => {
                let base = self.state.borrow().current_basedir();
                let full = self.source_path(interpreter, &first(), &base)?;
                self.include(interpreter, &full, false)?;
            }
            // Snakemake makes the working directory when it is missing.
            GlobalKeyword::WorkingDirectory => {
                let path = python_evaluator::modules::path_text(interpreter, &first())?;
                let resolved = interpreter.host.resolve(&path);
                python_evaluator::modules::make_dirs(interpreter, &resolved)?;
                interpreter.host.set_cwd(&resolved);
                self.state.borrow_mut().workdir = resolved;
            }
            GlobalKeyword::ConfigFile => {
                let path = python_evaluator::modules::path_text(interpreter, &first())?;
                self.configfile(interpreter, &path)?;
            }
            GlobalKeyword::RuleOrder => {
                let names: Vec<String> = args
                    .positional
                    .iter()
                    .filter_map(|name| name.as_str().map(str::to_owned))
                    .collect();
                let names: Vec<String> = names
                    .iter()
                    .map(|name| self.modified_rule_name(name))
                    .collect();
                self.state.borrow_mut().ruleorder.push(names);
            }
            GlobalKeyword::LocalRules => {
                let mut state = self.state.borrow_mut();
                for name in &args.positional {
                    if let Some(name) = name.as_str() {
                        state.localrules.push(name.to_owned());
                    }
                }
            }
            GlobalKeyword::WildcardConstraints => {
                let mut constraints = IndexMap::new();
                for (name, value) in &args.keywords {
                    constraints.insert(name.to_string(), interpreter.to_str(value)?);
                }
                let mut state = self.state.borrow_mut();
                let is_module = state.modifier().is_module;
                if let Some(modifier) = state.modifiers.last_mut() {
                    modifier.wildcard_constraints.extend(constraints.clone());
                }
                if !is_module {
                    state.global_constraints.extend(constraints);
                }
                let global = state.modifier().wildcard_constraints.clone();
                for rule in &mut state.rules {
                    rule.update_constraints(&global);
                }
            }
            GlobalKeyword::Container
            | GlobalKeyword::Singularity
            | GlobalKeyword::Containerized => {
                let mut state = self.state.borrow_mut();
                if let Some(modifier) = state.modifiers.last_mut() {
                    modifier.container = Some(first());
                    modifier.containerized = directive.keyword == GlobalKeyword::Containerized;
                }
            }
            GlobalKeyword::Conda => self.state.borrow_mut().global_conda = Some(first()),
            GlobalKeyword::ScatterGather => {
                let mut state = self.state.borrow_mut();
                for (name, value) in &args.keywords {
                    state
                        .scatter
                        .insert(name.to_string(), value.as_int().unwrap_or(1));
                }
                state
                    .global_constraints
                    .insert("scatteritem".into(), "\\d+-of-\\d+".into());
                if let Some(modifier) = state.modifiers.last_mut() {
                    modifier
                        .wildcard_constraints
                        .insert("scatteritem".into(), "\\d+-of-\\d+".into());
                }
            }
            // Variables that jobs get, which must be set, as `register_envvars` checks.
            GlobalKeyword::EnvironmentVariables => {
                let mut names = Vec::new();
                for name in &args.positional {
                    names.push(interpreter.to_str(name)?);
                }
                let invalid: Vec<String> = names
                    .iter()
                    .filter(|name| {
                        name.is_empty()
                            || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    })
                    .map(|name| python_evaluator::format::quote(name))
                    .collect();
                if !invalid.is_empty() {
                    return Err(workflow_error(format!(
                        "Invalid environment variables requested: {}. Environment variable names may only contain alphanumeric characters and the underscore. ",
                        invalid.join(", ")
                    )));
                }
                let environment = interpreter.host.environment();
                let undefined: Vec<&String> = names
                    .iter()
                    .filter(|name| !environment.iter().any(|(key, _)| key == *name))
                    .collect();
                if !undefined.is_empty() {
                    let undefined: Vec<&str> = undefined.iter().map(|name| name.as_str()).collect();
                    return Err(workflow_error(format!(
                        "The following environment variables are requested by the workflow but undefined. Please make sure that they are correctly defined before running Snakemake:\n{}",
                        undefined.join("\n")
                    )));
                }
                self.state.borrow_mut().envvars.extend(names);
            }
            GlobalKeyword::PepFile => {
                let path = python_evaluator::modules::path_text(interpreter, &first())?;
                let project = crate::pep::load(interpreter, &path)?;
                self.state.borrow().modifier().globals.set("pep", project);
                self.state.borrow_mut().pep = Some(crate::inputs::Pep {
                    file: path,
                    schema: None,
                });
            }
            // iunctus does not check the project against its schema, as eido does; the schema
            // describes the input `pepfile` (decision 24).
            GlobalKeyword::PepSchema => {
                if self.state.borrow().modifier().globals.get("pep").is_none() {
                    return Err(workflow_error(
                        "Please specify a PEP with the pepfile directive.",
                    ));
                }
                let path = python_evaluator::modules::path_text(interpreter, &first())?;
                if let Some(pep) = &mut self.state.borrow_mut().pep {
                    pep.schema = Some(path);
                }
            }
            GlobalKeyword::PathVariables => {
                let pairs: Vec<(Value, Value)> = args
                    .keywords
                    .iter()
                    .map(|(name, value)| (Value::str(&**name), value.clone()))
                    .collect();
                let pathvars = Pathvars::from_pairs(interpreter, &pairs, Level::Workflow)?;
                self.update_pathvars(&pathvars)?;
            }
            GlobalKeyword::Report
            | GlobalKeyword::OnStart
            | GlobalKeyword::OnSuccess
            | GlobalKeyword::OnError
            | GlobalKeyword::InputFlags
            | GlobalKeyword::OutputFlags
            | GlobalKeyword::ResourceScopes => {}
        }
        Ok(())
    }

    /// `configfile:`: merges a config file into the config, then the config from the
    /// command line again, which wins.
    pub fn configfile(&self, interpreter: &mut Interpreter, path: &str) -> Result<(), Error> {
        let (skip, config, overwrite, has_overwrite_files) = {
            let state = self.state.borrow();
            (
                state.modifier().skip_configfile,
                state
                    .modifier()
                    .globals
                    .get("config")
                    .unwrap_or(Value::None),
                state.overwrite_config.clone(),
                !state.overwrite_configfiles.is_empty(),
            )
        };
        if skip {
            return Ok(());
        }
        let resolved = interpreter.host.resolve(path);
        if interpreter.host.stat(&resolved).is_some() {
            let loaded = crate::config::load_config(interpreter, path)?;
            self.state.borrow_mut().configfiles.push(path.to_owned());
            crate::config::update(interpreter, &config, &loaded)?;
            crate::config::update(interpreter, &config, &overwrite)?;
            let pathvars = Pathvars::from_config(interpreter, &config, Level::Config)?;
            self.update_pathvars(&pathvars)?;
        } else if !has_overwrite_files {
            return Err(workflow_error(format!(
                "Workflow defines configfile {path} but it is not present or accessible (full checked path: {}).",
                resolved.display()
            )));
        } else {
            crate::config::update(interpreter, &config, &overwrite)?;
        }
        Ok(())
    }

    /// Takes path variables into those of the code that runs now.
    pub fn update_pathvars(&self, pathvars: &Pathvars) -> Result<(), Error> {
        let mut state = self.state.borrow_mut();
        match state.modifiers.last_mut() {
            Some(modifier) => modifier.pathvars.update(pathvars),
            None => Ok(()),
        }
    }

    fn modified_rule_name(&self, name: &str) -> String {
        let state = self.state.borrow();
        if let Some(found) = state.modifier().rule_names.borrow().get(name) {
            return found.clone();
        }
        name.to_owned()
    }

    /// Evaluates the directive values of a rule, in order.
    fn rule_info(
        &self,
        interpreter: &mut Interpreter,
        frame: &Frame,
        checkpoint: bool,
        name: Option<String>,
        directives: &[Directive<RuleKeyword>],
        docstrings: &[ast::Expr],
    ) -> Result<RuleInfo, Error> {
        let mut info = RuleInfo {
            name,
            checkpoint,
            ..RuleInfo::default()
        };
        if let Some(docstring) = docstrings.first() {
            let value = interpreter.eval(frame, docstring)?;
            info.docstring = value.as_str().map(|text| text.trim().to_owned());
        }
        for directive in directives {
            match &directive.value {
                DirectiveValue::Arguments(arguments) => {
                    let args = interpreter.eval_arguments(frame, arguments)?;
                    info.directives.push((directive.keyword, args));
                }
                DirectiveValue::Body(body) => {
                    let start = body
                        .first()
                        .map_or(directive.range.end(), |statement| statement.start());
                    let end = body
                        .last()
                        .map_or(directive.range.end(), |statement| statement.end());
                    let line_start = frame.source.text[..start.to_usize()]
                        .rfind('\n')
                        .map_or(0, |index| index + 1);
                    // The body with the indentation it has in the Snakefile, which Snakemake
                    // keeps; a body on the line of `run:` starts at its first statement.
                    let inline = !frame.source.text[line_start..start.to_usize()]
                        .trim()
                        .is_empty();
                    let from = if inline { start.to_usize() } else { line_start };
                    info.run = Some(frame.source.text[from..end.to_usize()].to_owned());
                    info.directives.push((directive.keyword, Args::default()));
                }
                DirectiveValue::Expression(expression) => {
                    let value = interpreter.eval(frame, expression)?;
                    info.directives
                        .push((directive.keyword, Args::new(vec![value])));
                }
                DirectiveValue::Names(names) => {
                    let values = names
                        .iter()
                        .map(|name| Value::str(name.text.as_str()))
                        .collect();
                    info.directives.push((directive.keyword, Args::new(values)));
                }
            }
        }
        // A directive that a rule gives twice keeps its first value: Snakemake applies the
        // decorators of the directives from the last one to the first.
        let mut seen: Vec<RuleKeyword> = Vec::new();
        info.directives.retain(|(keyword, _)| {
            let first = !seen.contains(keyword);
            seen.push(*keyword);
            first
        });
        if let Some((_, args)) = info
            .directives
            .iter()
            .find(|(keyword, _)| *keyword == RuleKeyword::Name)
        {
            let value = args.positional.first().cloned().unwrap_or(Value::None);
            info.name = Some(interpreter.to_str(&value)?);
        }
        Ok(info)
    }

    /// Builds a rule from its directive values and registers it, as `workflow.rule` does.
    fn define_rule(
        &self,
        interpreter: &mut Interpreter,
        mut info: RuleInfo,
        line: usize,
        snakefile: &str,
    ) -> Result<(), Error> {
        let original_name = {
            let state = self.state.borrow();
            info.name
                .clone()
                .unwrap_or_else(|| (state.rules.len() + 1).to_string())
        };
        let available = self.state.borrow().available_name(&original_name);
        let unused = available.is_none();
        let name = available.unwrap_or_else(|| {
            let count = self.state.borrow().unused.len() + 1;
            format!("{original_name} (unused {count})")
        });
        let skips: Vec<String> = {
            let state = self.state.borrow();
            let mut skips = Vec::new();
            if let Some(overwrite) = state.modifier().overwrite.as_ref().filter(|_| !unused) {
                for (keyword, property) in [
                    (RuleKeyword::Input, "input"),
                    (RuleKeyword::Output, "output"),
                    (RuleKeyword::Log, "log"),
                    (RuleKeyword::Benchmark, "benchmark"),
                ] {
                    if overwrite.has(keyword) {
                        skips.push(property.to_owned());
                    }
                }
                info.overwrite(overwrite);
            }
            skips
        };
        let (basedir, globals) = {
            let state = self.state.borrow();
            (state.current_basedir(), state.modifier().globals.clone())
        };
        let mut rule = Rule::new(name.clone(), basedir, snakefile.to_owned(), line, globals);
        // The rule's own path variables win over those of the code around it.
        let around = self.state.borrow().modifier().pathvars.clone();
        let pathvars = match info
            .directives
            .iter()
            .find(|(keyword, _)| *keyword == RuleKeyword::PathVariables)
        {
            Some((_, args)) => {
                let pairs: Vec<(Value, Value)> = args
                    .keywords
                    .iter()
                    .map(|(name, value)| (Value::str(&**name), value.clone()))
                    .collect();
                let mut own = Pathvars::from_pairs(interpreter, &pairs, Level::Rule)?;
                own.update(&around)?;
                own
            }
            None => around,
        };
        let pathvars = Rc::new(pathvars);
        rule.pathvars = pathvars.clone();
        rule.checkpoint = info.checkpoint;
        rule.docstring = info.docstring.clone();
        let mut default_target = false;
        for (keyword, args) in &info.directives {
            let first = args.positional.first().cloned().unwrap_or(Value::None);
            match keyword {
                RuleKeyword::Name => {}
                RuleKeyword::Input | RuleKeyword::Output | RuleKeyword::Log => {
                    let (items, property, output) = match keyword {
                        RuleKeyword::Input => (&mut rule.input, "input", false),
                        RuleKeyword::Output => (&mut rule.output, "output", true),
                        _ => (&mut rule.log, "log", false),
                    };
                    for value in &args.positional {
                        let multiext = self.multiext_names(interpreter, value)?;
                        match multiext {
                            Some(names) => {
                                for (file, file_name) in names {
                                    self.add_item(
                                        interpreter,
                                        items,
                                        &file,
                                        file_name.as_deref(),
                                        property,
                                        output,
                                        &skips,
                                        &pathvars,
                                    )?;
                                }
                            }
                            None => self.add_item(
                                interpreter,
                                items,
                                value,
                                None,
                                property,
                                output,
                                &skips,
                                &pathvars,
                            )?,
                        }
                    }
                    for (key, value) in &args.keywords {
                        self.add_item(
                            interpreter,
                            items,
                            value,
                            Some(key),
                            property,
                            output,
                            &skips,
                            &pathvars,
                        )?;
                    }
                }
                RuleKeyword::Benchmark => {
                    let value = match first.plain() {
                        Value::Native(object) if object.type_name() == "PosixPath" => {
                            Value::str(interpreter.to_str(&first)?)
                        }
                        _ => first.clone(),
                    };
                    let modified = self.state.borrow().modify_path(&value, "benchmark", &skips);
                    rule.benchmark = Some(apply_pathvars(&pathvars, &modified)?);
                }
                RuleKeyword::Parameters => {
                    let mut params = Items::default();
                    for value in &args.positional {
                        params.push(value.clone());
                    }
                    for (key, value) in &args.keywords {
                        params.push(value.clone());
                        params.add_name(key);
                    }
                    rule.params = params;
                }
                RuleKeyword::Threads => rule.threads = Some(first),
                RuleKeyword::Resources => {
                    if !args.positional.is_empty() {
                        return Err(workflow_error("Resources have to be named."));
                    }
                    for (key, value) in &args.keywords {
                        rule.resources.insert(key.to_string(), value.clone());
                    }
                }
                RuleKeyword::Retries => rule.retries = Some(first),
                RuleKeyword::Priority => rule.priority = Some(first),
                RuleKeyword::Message => rule.message = Some(first),
                RuleKeyword::Conda => rule.conda = Some(first).filter(|value| !value.is_none()),
                RuleKeyword::Container | RuleKeyword::Singularity | RuleKeyword::Containerized => {
                    rule.container = if matches!(first.plain(), Value::Bool(false)) {
                        Some(None)
                    } else if first.is_none() {
                        None
                    } else {
                        Some(Some(first))
                    };
                }
                RuleKeyword::EnvironmentModules => rule.envmodules = args.positional.clone(),
                RuleKeyword::WildcardConstraints => {
                    for (key, value) in &args.keywords {
                        rule.wildcard_constraints
                            .insert(key.to_string(), interpreter.to_str(value)?);
                    }
                }
                RuleKeyword::Group => rule.group = Some(first),
                RuleKeyword::LocalRule => rule.localrule = interpreter.truthy(&first)?,
                RuleKeyword::DefaultTarget => {
                    if !matches!(first.plain(), Value::Bool(_)) {
                        return Err(workflow_error(
                            "Invalid argument for 'default_target:' directive. Only True allowed. Do not use the directive for rules that shall not be the default target. ",
                        ));
                    }
                    default_target = matches!(first.plain(), Value::Bool(true));
                }
                RuleKeyword::Shadow | RuleKeyword::Cache | RuleKeyword::Handover => {}
                RuleKeyword::PathVariables => {}
                RuleKeyword::Shell => rule.action = Action::Shell(first),
                RuleKeyword::Script => rule.action = Action::Script(first),
                RuleKeyword::Notebook => rule.action = Action::Notebook(first),
                RuleKeyword::Wrapper => {
                    // An old meta-wrapper names its wrappers by `master/`, meaning its own tag.
                    let tag = self.state.borrow().modifier().wrapper_tag.clone();
                    let wrapper = match (tag, first.as_str()) {
                        (Some(tag), Some(name))
                            if name.starts_with("master/") && !name.contains("://") =>
                        {
                            Value::str(format!("{tag}/{}", &name["master/".len()..]))
                        }
                        _ => first,
                    };
                    rule.action = Action::Wrapper(wrapper);
                }
                RuleKeyword::TemplateEngine => rule.action = Action::TemplateEngine(first),
                RuleKeyword::Cwl => rule.action = Action::Cwl(first),
                RuleKeyword::Run => rule.action = Action::Run(info.run.clone().unwrap_or_default()),
            }
        }
        {
            let state = self.state.borrow();
            if rule.container.is_none()
                && let Some(container) = &state.modifier().container
                && !matches!(rule.action, Action::TemplateEngine(_))
            {
                rule.container = Some(Some(container.clone()));
            }
            let constraints = state.modifier().wildcard_constraints.clone();
            rule.update_constraints(&constraints);
        }
        self.check_wildcards(&rule)?;
        let mut state = self.state.borrow_mut();
        if unused {
            if let Some(modifier) = state.modifiers.last() {
                modifier.rule_names.borrow_mut().insert(original_name, name);
            }
            state.unused.push(rule);
            return Ok(());
        }
        let allow_overwrite = state
            .modifiers
            .iter()
            .any(|modifier| modifier.allow_overwrite);
        if let Some(index) = state.rule_index(&name) {
            if !allow_overwrite {
                return Err(workflow_error(format!(
                    "The name {name} is already used by another rule"
                )));
            }
            state.rules[index] = rule;
            state.infos[index] = info;
        } else {
            state.rules.push(rule);
            info.name = Some(original_name.clone());
            state.infos.push(info);
        }
        if let Some(modifier) = state.modifiers.last() {
            modifier
                .rule_names
                .borrow_mut()
                .insert(original_name, name.clone());
        }
        if state.default_target.is_none() || default_target {
            state.default_target = Some(name);
        }
        Ok(())
    }

    /// Output, log and benchmark files of a rule must name the same wildcards.
    fn check_wildcards(&self, rule: &Rule) -> Result<(), Error> {
        let mut expected: Option<Vec<String>> = None;
        let mut products: Vec<String> = Vec::new();
        for value in rule.output.values.iter().chain(rule.log.values.iter()) {
            if let Some(text) = value.as_str() {
                products.push(text.to_owned());
            }
        }
        for product in products {
            let mut names = crate::pattern::wildcard_names(&product);
            names.sort();
            match &expected {
                None => expected = Some(names),
                Some(existing) if *existing != names => {
                    return Err(workflow_error(format!(
                        "Not all output, log and benchmark files of rule {} contain the same wildcards. This is crucial though, in order to avoid that two or more jobs write to the same file.",
                        rule.name
                    )));
                }
                Some(_) => {}
            }
        }
        Ok(())
    }

    /// The files and names of a `multiext` list, if the value is one.
    fn multiext_names(
        &self,
        interpreter: &mut Interpreter,
        value: &Value,
    ) -> Result<Option<NamedFiles>, Error> {
        let Value::List(items) = value.plain() else {
            return Ok(None);
        };
        let items = items.borrow().clone();
        if items.is_empty() || !items.iter().all(|item| item.tag("multiext").is_some()) {
            return Ok(None);
        }
        let mut named = Vec::new();
        for item in items {
            let flag = item.tag("multiext").cloned().unwrap_or(Value::None);
            let name = interpreter
                .get_item(&flag, &Value::str("name"))
                .ok()
                .filter(|name| !name.is_none());
            let name = match name {
                Some(name) => Some(interpreter.to_str(&name)?),
                None => None,
            };
            named.push((item, name));
        }
        Ok(Some(named))
    }

    /// Adds an input, output or log item as `_set_inoutput_item` does: texts and paths,
    /// functions for inputs and logs, and the items of lists under one name.
    #[allow(clippy::too_many_arguments)]
    fn add_item(
        &self,
        interpreter: &mut Interpreter,
        items: &mut Items,
        value: &Value,
        name: Option<&str>,
        property: &str,
        output: bool,
        skips: &[String],
        pathvars: &Pathvars,
    ) -> Result<(), Error> {
        match value.plain() {
            Value::Str(_) => {
                let modified = self.state.borrow().modify_path(value, property, skips);
                items.push(apply_pathvars(pathvars, &modified)?);
                if let Some(name) = name {
                    items.add_name(name);
                }
            }
            // Paths, also of Python packages, like `upath.UPath`, as Snakemake takes them.
            Value::Native(object)
                if object.type_name() == "PosixPath"
                    || python_evaluator::packages::fspath(interpreter, value)?.is_some() =>
            {
                let text = match python_evaluator::packages::fspath(interpreter, value)? {
                    Some(path) => path,
                    None => interpreter.to_str(value)?,
                };
                let text = retag(value, Value::str(text));
                let modified = self.state.borrow().modify_path(&text, property, skips);
                items.push(apply_pathvars(pathvars, &modified)?);
                if let Some(name) = name {
                    items.add_name(name);
                }
            }
            _ if functions::is_callable(interpreter, value) => {
                if output {
                    return Err(workflow_error(
                        "Only input files can be specified as functions",
                    ));
                }
                items.push(value.clone());
                if let Some(name) = name {
                    items.add_name(name);
                }
            }
            Value::List(_)
            | Value::Tuple(_)
            | Value::Set(_)
            | Value::FrozenSet(_)
            | Value::Iterator(_)
            | Value::Dict(_)
            | Value::Native(_) => {
                let sub = interpreter.iterate(value).map_err(|_| {
                    workflow_error("Input and output files have to be specified as strings or lists of strings.")
                })?;
                let start = items.values.len();
                for item in sub {
                    let item = match value.tags() {
                        Some(tags) => {
                            let mut item = item;
                            for (flag, flag_value) in tags {
                                if item.tag(flag).is_none() {
                                    item = item.with_tag(flag, flag_value.clone());
                                }
                            }
                            item
                        }
                        None => item,
                    };
                    self.add_item(
                        interpreter,
                        items,
                        &item,
                        None,
                        property,
                        output,
                        skips,
                        pathvars,
                    )?;
                }
                if let Some(name) = name {
                    items.set_name(name, start, Some(items.values.len()));
                }
            }
            _ => {
                return Err(workflow_error(
                    "Input and output files have to be specified as strings or lists of strings.",
                ));
            }
        }
        Ok(())
    }

    fn use_rule(
        &self,
        interpreter: &mut Interpreter,
        frame: &Frame,
        statement: &UseRule,
    ) -> Result<(), Error> {
        let with = match &statement.with {
            Some(directives) => Some(self.rule_info(
                interpreter,
                frame,
                false,
                None,
                directives,
                &statement.docstrings,
            )?),
            None => None,
        };
        let names: Vec<String> = statement
            .rules
            .iter()
            .map(|name| name.text.clone())
            .collect();
        let alias = statement.alias.as_ref().map(|alias| alias.text.clone());
        let line = frame.source.line(statement.range.start());
        match &statement.module {
            None => {
                // Local inheritance from a rule defined before.
                if names.len() > 1 {
                    return Err(workflow_error(
                        "'use rule' statement from rule in the same module must declare a single rule but multiple rules are declared.",
                    ));
                }
                // A module statement that takes other rules skips this one, as Snakemake does.
                let new_name = alias.as_deref().unwrap_or(&names[0]);
                if self.state.borrow().available_name(new_name).is_none() {
                    return Ok(());
                }
                let original = self.modified_rule_name(&names[0]);
                let mut info = {
                    let state = self.state.borrow();
                    let Some(index) = state.rule_index(&original) else {
                        return Err(workflow_error(format!(
                            "Rule {} is not defined in this workflow.",
                            names[0]
                        )));
                    };
                    state.infos[index].clone()
                };
                if let Some(with) = &with {
                    info.overwrite(with);
                }
                info.name = alias.clone().or(Some(names[0].clone()));
                self.define_rule(interpreter, info, line, &frame.source.path)
            }
            Some(module) => self.use_module_rules(
                interpreter,
                &module.text,
                &names,
                &statement
                    .exclude
                    .iter()
                    .map(|name| name.text.clone())
                    .collect::<Vec<_>>(),
                alias,
                with,
            ),
        }
    }

    fn module(
        &self,
        interpreter: &mut Interpreter,
        frame: &Frame,
        module: &Module,
    ) -> Result<(), Error> {
        let mut spec = ModuleSpec {
            name: module
                .name
                .as_ref()
                .map(|name| name.text.clone())
                .unwrap_or_default(),
            snakefile: None,
            meta_wrapper: None,
            config: None,
            prefix: None,
            replace_prefix: Vec::new(),
            skip_validation: false,
            basedir: self.state.borrow().current_basedir(),
            pathvars: Vec::new(),
        };
        for setting in &module.settings {
            let value = match &setting.value {
                DirectiveValue::Expression(expression) => interpreter.eval(frame, expression)?,
                DirectiveValue::Arguments(arguments) => {
                    let args = interpreter.eval_arguments(frame, arguments)?;
                    Value::dict_of(
                        args.keywords
                            .into_iter()
                            .map(|(key, value)| (key.to_string(), value)),
                    )
                }
                _ => Value::None,
            };
            match setting.keyword {
                ModuleKeyword::Name => spec.name = interpreter.to_str(&value)?,
                ModuleKeyword::Snakefile => spec.snakefile = Some(value),
                ModuleKeyword::MetaWrapper => spec.meta_wrapper = Some(value),
                ModuleKeyword::Config => spec.config = Some(value).filter(|value| !value.is_none()),
                ModuleKeyword::SkipValidation => {
                    spec.skip_validation = interpreter.truthy(&value)?
                }
                ModuleKeyword::Prefix => {
                    spec.prefix = Some(python_evaluator::modules::path_text(interpreter, &value)?)
                }
                ModuleKeyword::ReplacePrefix => {
                    for (from, to) in interpreter.mapping_items(&value)? {
                        spec.replace_prefix
                            .push((interpreter.to_str(&from)?, interpreter.to_str(&to)?));
                    }
                }
                ModuleKeyword::PathVariables => {
                    spec.pathvars = interpreter.mapping_items(&value)?;
                }
            }
        }
        let mut state = self.state.borrow_mut();
        if let Some(modifier) = state.modifiers.last_mut() {
            modifier.modules.insert(spec.name.clone(), spec);
        }
        Ok(())
    }

    /// The module that `use rule ... from <name>` names, with the alias of its rules. The name
    /// may also be a variable of the Snakefile that holds the name of a module, and then the
    /// alias is one too, as in Snakemake's dynamic module names.
    fn resolve_module(
        &self,
        interpreter: &mut Interpreter,
        module_name: &str,
        alias: Option<String>,
    ) -> Result<(String, Option<String>, ModuleSpec), Error> {
        let find = |name: &str| {
            let state = self.state.borrow();
            state
                .modifiers
                .iter()
                .rev()
                .find_map(|modifier| modifier.modules.get(name).cloned())
        };
        if let Some(spec) = find(module_name) {
            return Ok((module_name.to_owned(), alias, spec));
        }
        let globals = self.state.borrow().modifier().globals.clone();
        let Some(value) = globals.get(module_name) else {
            return Err(workflow_error(format!(
                "Module {module_name} has not been registered with 'module' statement before using it in 'use rule' statement."
            )));
        };
        let resolved = interpreter.to_str(&value)?;
        let spec = find(&resolved).ok_or_else(|| {
            workflow_error(format!(
                "Dynamic module name '{module_name}' resolves to '{resolved}', but has not been registered with a 'module' statement."
            ))
        })?;
        let alias = match alias {
            Some(alias) => {
                let variable = alias.strip_suffix('*').unwrap_or(&alias);
                let value = globals.get(variable).ok_or_else(|| {
                    workflow_error(format!(
                        "Module alias {alias} not in current frame to resolve dynamic module {resolved} in 'use rule'."
                    ))
                })?;
                let text = interpreter.to_str(&value)?;
                Some(if alias.ends_with('*') {
                    format!("{text}*")
                } else {
                    text
                })
            }
            None => None,
        };
        Ok((resolved, alias, spec))
    }

    fn use_module_rules(
        &self,
        interpreter: &mut Interpreter,
        module_name: &str,
        rules: &[String],
        exclude: &[String],
        alias: Option<String>,
        with: Option<RuleInfo>,
    ) -> Result<(), Error> {
        let (module_name, alias, spec) = self.resolve_module(interpreter, module_name, alias)?;
        let module_name = module_name.as_str();
        let (full, wrapper_tag) = match (&spec.meta_wrapper, &spec.snakefile) {
            (Some(meta_wrapper), _) => {
                let meta_wrapper = interpreter.to_str(meta_wrapper)?;
                self.meta_wrapper(interpreter, &meta_wrapper)?
            }
            (None, Some(snakefile)) => (
                self.source_path(interpreter, snakefile, &spec.basedir)?,
                None,
            ),
            (None, None) => {
                return Err(workflow_error(
                    "Module statement must either define snakefile or meta_wrapper to use.",
                ));
            }
        };
        let all = rules.iter().any(|rule| rule == "*");
        if alias.as_ref().is_some_and(|alias| !alias.contains('*')) && (all || rules.len() > 1) {
            return Err(Error::new(
                "SyntaxError",
                "Multiple rules in 'use rule' statement but name modification ('as' statement) does not contain a wildcard '*'.",
            ));
        }
        let globals = interpreter.new_globals(module_name, None);
        let state_rc = self.state.clone();
        functions::install(&globals, &state_rc);
        crate::namespace::install(interpreter, &globals)?;
        let config = spec
            .config
            .clone()
            .unwrap_or_else(|| Value::dict(Dict::default()));
        globals.set("config", config);
        let rule_names = Rc::new(RefCell::new(IndexMap::new()));
        globals.set(
            "rules",
            Value::native(crate::objects::RulesObject::scoped(
                self.state.clone(),
                rule_names.clone(),
            )),
        );
        globals.set(
            "checkpoints",
            Value::native(crate::objects::CheckpointsObject::scoped(
                self.state.clone(),
                rule_names.clone(),
            )),
        );
        // The module's path variables: those of the code that loads it, then those of the
        // module statement and of its config.
        let mut pathvars = self.state.borrow().modifier().pathvars.clone();
        pathvars.update(&Pathvars::from_pairs(
            interpreter,
            &spec.pathvars,
            Level::Module,
        )?)?;
        if let Some(config) = &spec.config {
            pathvars.update(&Pathvars::from_config(interpreter, config, Level::Module)?)?;
        }
        let modifier = Modifier {
            globals: globals.clone(),
            rule_names: rule_names.clone(),
            name_modifier: alias.clone(),
            whitelist: if all { None } else { Some(rules.to_vec()) },
            exclude: exclude.to_vec(),
            prefix: spec.prefix.clone(),
            replace_prefix: spec.replace_prefix.clone(),
            overwrite: with,
            skip_configfile: spec.config.is_some(),
            skip_validation: spec.skip_validation,
            wildcard_constraints: IndexMap::new(),
            container: None,
            containerized: false,
            is_module: true,
            modules: IndexMap::new(),
            allow_overwrite: !all,
            pathvars,
            wrapper_tag,
        };
        self.state.borrow_mut().modifiers.push(modifier);
        let result = self.include(interpreter, &full, true);
        let modifier = self
            .state
            .borrow_mut()
            .modifiers
            .pop()
            .expect("the module modifier");
        result?;
        // The rules of the module become known to the code that uses it by their new names.
        // Each rule of the module, used or not, by its name with the alias, as Snakemake's
        // `inherit_rule_proxies` does.
        let parent_names = self.state.borrow().modifier().rule_names.clone();
        for (original, global_name) in modifier.rule_names.borrow().iter() {
            let local = match &alias {
                Some(pattern) if pattern.contains('*') => pattern.replace('*', original),
                Some(pattern) if rules.contains(original) => pattern.clone(),
                Some(_) => continue,
                None => original.clone(),
            };
            parent_names.borrow_mut().insert(local, global_name.clone());
        }
        // The module's globals are reachable by its name.
        let module = Value::Module(Rc::new(python_evaluator::value::Module {
            name: module_name.into(),
            globals,
        }));
        let parent_globals = self.state.borrow().modifier().globals.clone();
        parent_globals.set(module_name, module);
        Ok(())
    }
}
