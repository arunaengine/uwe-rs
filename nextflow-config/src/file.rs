//! Evaluating one config file: its statements run from top to bottom into a tree.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::{Error, Host, Interpreter};
use nextflow_evaluator::lower::{
    ConfigNames, lower_config_function, lower_config_statement, lower_legacy_config_value,
};
use nextflow_evaluator::lowered::{Code, Link, Module};
use nextflow_evaluator::number::Number;
use nextflow_evaluator::value::{PathValue, Value};
use nextflow_parser::ast::{
    Access, AssignOperator, ConfigCatch, ConfigStatement, Expression, ExpressionKind, Literal,
    Name, Statement,
};
use nextflow_parser::{Diagnostic, LineIndex, Span, parse_config};

use crate::tree;
use crate::{ConfigError, Options};

pub(crate) struct Evaluator<'a> {
    options: &'a Options,
    host: &'a mut dyn Host,
    /// The names of all profiles defined in the files and includes that ran.
    pub declared_profiles: BTreeSet<String>,
    sources: HashMap<PathBuf, Rc<str>>,
}

/// The file being evaluated.
struct File {
    path: PathBuf,
    source: Rc<str>,
    tree: Value,
    /// The module that closures of this file belong to.
    module: Rc<Module>,
    /// The functions of legacy syntax that the file defines.
    functions: HashSet<Rc<str>>,
    /// The functions of the files that include this one, which the module links.
    inherited: HashSet<Rc<str>>,
    /// The local variables of legacy syntax, one scope per block, innermost last.
    variables: Vec<Vec<(Rc<str>, Value)>>,
}

impl File {
    fn variable(&self, name: &str) -> Option<Value> {
        self.variables
            .iter()
            .rev()
            .flat_map(|scope| scope.iter().rev())
            .find(|(declared, _)| &**declared == name)
            .map(|(_, value)| value.clone())
    }

    /// Sets a declared variable; the result is whether it exists.
    fn set_variable(&mut self, name: &str, value: &Value) -> bool {
        let found = self
            .variables
            .iter_mut()
            .rev()
            .flat_map(|scope| scope.iter_mut().rev())
            .find(|(declared, _)| &**declared == name);
        match found {
            Some((_, slot)) => {
                *slot = value.clone();
                true
            }
            None => false,
        }
    }

    fn declare(&mut self, name: &str, value: Value) {
        let scope = self.variables.last_mut().expect("a scope is open");
        scope.push((name.into(), value));
    }
}

impl<'a> Evaluator<'a> {
    pub fn new(options: &'a Options, host: &'a mut dyn Host) -> Self {
        Self {
            options,
            host,
            declared_profiles: BTreeSet::new(),
            sources: HashMap::new(),
        }
    }

    /// A file given to the run. It sees only the command-line params, not those of other files.
    pub fn top_level(&mut self, path: &Path) -> Result<Value, ConfigError> {
        let params = tree::deep_copy(&self.options.params);
        self.file(path, params, None)
    }

    /// Evaluates a file whose `params` start as `params`, and returns its tree. The functions
    /// of the including file can be called in it.
    fn file(
        &mut self,
        path: &Path,
        params: Value,
        includer: Option<&File>,
    ) -> Result<Value, ConfigError> {
        let source = self.source(path)?;
        let parsed = parse_config(&source);
        if !parsed.errors.is_empty() {
            return Err(located(path, &source, &parsed.errors));
        }
        let tree = tree::map();
        tree::assign(&tree, &["params".into()], params).expect("a new tree");
        let mut file = File {
            path: path.to_owned(),
            source,
            tree,
            module: Rc::new(Module::default()),
            functions: HashSet::new(),
            inherited: HashSet::new(),
            variables: vec![Vec::new()],
        };
        file.module = self.module(&mut file, &parsed.tree.statements, includer)?;
        *file.module.source.borrow_mut() = Some(file.source.clone());
        self.statements(&mut file, &parsed.tree.statements, &[], false)?;
        Ok(file.tree)
    }

    /// The module of a file: the functions of legacy syntax that it defines, which Groovy
    /// knows before the first statement runs, and links to the functions of the including
    /// files.
    fn module(
        &mut self,
        file: &mut File,
        statements: &[ConfigStatement],
        includer: Option<&File>,
    ) -> Result<Rc<Module>, ConfigError> {
        let functions: Vec<_> = statements
            .iter()
            .filter_map(|statement| match statement {
                ConfigStatement::Function(function) => Some(function),
                _ => None,
            })
            .collect();
        file.functions = functions
            .iter()
            .map(|function| function.name.text.as_str().into())
            .collect();
        let mut module = Module::default();
        if let Some(includer) = includer {
            for name in includer.functions.iter().chain(&includer.inherited) {
                if file.functions.contains(name) {
                    continue;
                }
                file.inherited.insert(name.clone());
                let link = Link {
                    module: includer.module.clone(),
                    name: name.clone(),
                };
                module.links.borrow_mut().insert(name.clone(), link);
            }
        }
        let names = ConfigNames {
            functions: file.functions.clone(),
            inherited: file.inherited.clone(),
            lookup: None,
        };
        for function in functions {
            let code = lower_config_function(function, &file.source, &names)
                .map_err(|errors| located(&file.path, &file.source, &errors))?;
            let name: Rc<str> = function.name.text.as_str().into();
            module
                .function_names
                .entry(name)
                .or_default()
                .push(module.functions.len());
            module.functions.push(Rc::new(code));
        }
        Ok(Rc::new(module))
    }

    fn source(&mut self, path: &Path) -> Result<Rc<str>, ConfigError> {
        if let Some(source) = self.sources.get(path) {
            return Ok(source.clone());
        }
        let text = path.to_string_lossy();
        let bytes = if text.contains("://") {
            let remote = PathValue::parse(&text, "/");
            self.host
                .read(&remote)
                .map_err(|error| ConfigError::Failed {
                    file: path.to_owned(),
                    error,
                })?
        } else {
            std::fs::read(path).map_err(|error| ConfigError::Invalid {
                messages: vec![format!(
                    "Config file does not exist: {} ({error})",
                    path.display()
                )],
            })?
        };
        let source: Rc<str> = String::from_utf8_lossy(&bytes).into();
        self.sources.insert(path.to_owned(), source.clone());
        Ok(source)
    }

    /// Runs statements in a new scope of local variables.
    fn block(
        &mut self,
        file: &mut File,
        statements: &[ConfigStatement],
        prefix: &[Rc<str>],
        in_profile: bool,
    ) -> Result<(), ConfigError> {
        let depth = file.variables.len();
        file.variables.push(Vec::new());
        let result = self.statements(file, statements, prefix, in_profile);
        file.variables.truncate(depth);
        result
    }

    /// Runs statements below the scope `prefix`. A profile body runs at the top level.
    fn statements(
        &mut self,
        file: &mut File,
        statements: &[ConfigStatement],
        prefix: &[Rc<str>],
        in_profile: bool,
    ) -> Result<(), ConfigError> {
        for statement in statements {
            match statement {
                // Legacy syntax: an include inside a block, whose settings the legacy parser puts
                // into the block.
                ConfigStatement::Include { path, .. } => self.include(file, path, prefix)?,
                ConfigStatement::Assign { path, value, span } => {
                    self.assign(file, path, value, prefix, *span)?;
                }
                ConfigStatement::Block { name, body, .. }
                    if name.text == "profiles" && prefix.is_empty() =>
                {
                    self.profiles(file, body)?;
                }
                ConfigStatement::Block { name, body, .. } => {
                    let mut inner = prefix.to_vec();
                    inner.push(name.text.as_str().into());
                    // A block replaces a null setting of its name with its settings, as the
                    // legacy parser does, like `spot_interval_beds { ... }` of nf-core/ssds
                    // after `spot_interval_beds = null`.
                    let names: Vec<&str> = inner.iter().map(|name| &**name).collect();
                    if let Some(Value::Null) = tree::get(&file.tree, &names) {
                        tree::assign(&file.tree, &inner, tree::map())
                            .expect("the parents are maps");
                    }
                    self.block(file, body, &inner, false)?;
                }
                ConfigStatement::Apply { name, calls, span } => {
                    if name.text != "plugins" {
                        let message = "Config settings must be assigned with an equals sign (`=`)";
                        return Err(invalid(file, message, *span));
                    }
                    // Only a `plugins` block at the top level of a file counts.
                    if prefix.is_empty() && !in_profile {
                        let mut ids = Vec::new();
                        for call in calls {
                            for argument in &call.arguments {
                                if let nextflow_parser::ast::ExpressionKind::Literal(
                                    Literal::String(id),
                                ) = &argument.value.kind
                                {
                                    ids.push(Value::string(id.as_str()));
                                }
                            }
                        }
                        let set = Value::set(ids);
                        tree::assign(&file.tree, &["plugins".into()], set)
                            .map_err(|message| invalid(file, &message, *span))?;
                    }
                }
                ConfigStatement::Selector {
                    kind,
                    target,
                    body,
                    span,
                } => {
                    // Legacy syntax: the legacy parser makes a selector at the top level of a
                    // file a block like any other, which no process reads.
                    let top = prefix.is_empty() && !in_profile;
                    if !top && (prefix.len() != 1 || &*prefix[0] != "process") {
                        let message = "Config selectors are only allowed in the `process` scope";
                        return Err(invalid(file, message, *span));
                    }
                    if kind.text != "withName" && kind.text != "withLabel" {
                        let message = format!("Unknown config selector `{}`", kind.text);
                        return Err(invalid(file, &message, kind.span));
                    }
                    let scope: Rc<str> = format!("{}:{}", kind.text, target.text).into();
                    let mut inner = prefix.to_vec();
                    inner.push(scope);
                    self.block(file, body, &inner, false)?;
                }
                ConfigStatement::If {
                    condition,
                    then,
                    otherwise,
                    ..
                } => {
                    let condition = self.value(file, condition, prefix, process_scope(prefix))?;
                    let branch = if condition.truth() { then } else { otherwise };
                    self.block(file, branch, prefix, in_profile)?;
                }
                ConfigStatement::Try { body, catches, .. } => {
                    if let Err(error) = self.block(file, body, prefix, in_profile) {
                        self.catch(file, error, catches, prefix, in_profile)?;
                    }
                }
                ConfigStatement::Declaration {
                    names,
                    value,
                    destructure,
                    ..
                } => {
                    let value = match value {
                        Some(value) => self.value(file, value, prefix, process_scope(prefix))?,
                        None => Value::Null,
                    };
                    if !*destructure {
                        file.declare(&names[0].text, value);
                        continue;
                    }
                    for (index, name) in names.iter().enumerate() {
                        let item = match &value {
                            Value::List(list) => list.borrow().items.get(index).cloned(),
                            _ => None,
                        };
                        file.declare(&name.text, item.unwrap_or(Value::Null));
                    }
                }
                // Functions are defined before the first statement runs.
                ConfigStatement::Function(_) => {}
                ConfigStatement::Statement(statement) => {
                    self.statement(file, statement, prefix)?;
                }
            }
        }
        Ok(())
    }

    /// Runs the first `catch` clause that takes the exception of a failed `try` body. Only a
    /// thrown exception can be caught, never a feature that iunctus does not support.
    /// `path = value`, which gives the value. Legacy syntax sets each setting of a chain like
    /// `a = b = 5` to the value of the last one.
    fn assign(
        &mut self,
        file: &mut File,
        path: &[Name],
        value: &Expression,
        prefix: &[Rc<str>],
        span: Span,
    ) -> Result<Value, ConfigError> {
        let mut full: Vec<Rc<str>> = prefix.to_vec();
        full.extend(path.iter().map(|name| Rc::from(name.text.as_str())));
        let process = process_scope(&full);
        let config = match &value.kind {
            ExpressionKind::Assign {
                target,
                operator: AssignOperator::Assign,
                value: last,
            } if chained_path(target).is_some() => {
                let names = chained_path(target).unwrap_or_default();
                self.assign(file, &names, last, prefix, span)?
            }
            _ => self.value(file, value, prefix, process)?,
        };
        // Legacy syntax: assigning a local variable.
        if let [name] = path
            && file.variable(&name.text).is_some()
        {
            file.set_variable(&name.text, &config);
            return Ok(config);
        }
        let setting = match self.cli_param(&full) {
            Some(cli) => convert(&cli, &config),
            None => config.clone(),
        };
        tree::assign(&file.tree, &full, setting)
            .map_err(|message| invalid(file, &message, span))?;
        Ok(config)
    }

    fn catch(
        &mut self,
        file: &mut File,
        error: ConfigError,
        catches: &[ConfigCatch],
        prefix: &[Rc<str>],
        in_profile: bool,
    ) -> Result<(), ConfigError> {
        let Some(exception) = exception(&error) else {
            return Err(error);
        };
        let caught = catches.iter().find(|catch| {
            let classes = catch.types.iter().filter_map(|name| name.parts.last());
            match catch.types.is_empty() {
                true => exception.is_instance("Exception"),
                false => classes
                    .map(|name| name.text.as_str())
                    .any(|class| exception.is_instance(class)),
            }
        });
        let Some(catch) = caught else {
            return Err(error);
        };
        let depth = file.variables.len();
        file.variables.push(Vec::new());
        file.declare(&catch.name.text, Value::Exception(exception));
        let result = self.statements(file, &catch.body, prefix, in_profile);
        file.variables.truncate(depth);
        result
    }

    /// A `profiles` block: the selected profiles it defines run at its end, in the order of
    /// `-profile`. A profile defined twice in one block keeps its last body.
    fn profiles(&mut self, file: &mut File, body: &[ConfigStatement]) -> Result<(), ConfigError> {
        let mut defined: HashMap<&str, &[ConfigStatement]> = HashMap::new();
        for statement in body {
            match statement {
                ConfigStatement::Block { name, body, .. } => {
                    defined.insert(&name.text, body);
                    self.declared_profiles.insert(name.text.clone());
                }
                // Legacy syntax: the legacy parser includes the file as at the top, with its
                // profiles.
                ConfigStatement::Include { path, .. } => self.include(file, path, &[])?,
                ConfigStatement::Assign { span, .. }
                | ConfigStatement::Apply { span, .. }
                | ConfigStatement::Selector { span, .. }
                | ConfigStatement::If { span, .. }
                | ConfigStatement::Try { span, .. }
                | ConfigStatement::Declaration { span, .. } => {
                    let message = "Only profile blocks are allowed in the `profiles` scope";
                    return Err(invalid(file, message, *span));
                }
                ConfigStatement::Function(function) => {
                    let message = "Only profile blocks are allowed in the `profiles` scope";
                    return Err(invalid(file, message, function.span));
                }
                ConfigStatement::Statement(statement) => {
                    let message = "Only profile blocks are allowed in the `profiles` scope";
                    return Err(invalid(file, message, statement.span));
                }
            }
        }
        let standard = vec!["standard".to_owned()];
        let selected = self.options.profiles.clone().unwrap_or(standard);
        for name in &selected {
            if let Some(body) = defined.get(name.as_str()) {
                self.block(file, body, &[], true)?;
            }
        }
        Ok(())
    }

    /// `includeConfig path`: the file runs with a copy of the current params, and its tree
    /// merges into this one, inside the block `prefix`.
    fn include(
        &mut self,
        file: &mut File,
        path: &Expression,
        prefix: &[Rc<str>],
    ) -> Result<(), ConfigError> {
        let target = self.value(file, path, prefix, false)?;
        let text = target.to_string();
        if !target.truth() {
            let exception =
                Exception::assertion(format!("assert includeFile\n       |\n       {target}"));
            return Err(failed(file, exception, path.span));
        }
        if text == "/dev/null" {
            return Ok(());
        }
        let base = file.path.parent().unwrap_or(Path::new("/"));
        let included = match text.contains("://") {
            true => PathBuf::from(&text),
            false => base.join(&text),
        };
        let params = tree::get(&file.tree, &["params"]).unwrap_or_else(tree::map);
        let mut tree = self.file(&included, tree::deep_copy(&params), Some(file))?;
        if !prefix.is_empty() {
            let nested = tree::map();
            tree::assign(&nested, prefix, tree)
                .map_err(|message| invalid(file, &message, path.span))?;
            tree = nested;
        }
        tree::deep_merge(&file.tree, &tree);
        Ok(())
    }

    /// The command-line value of `params.<name>`, for a setting with exactly two parts.
    fn cli_param(&self, path: &[Rc<str>]) -> Option<Value> {
        match path {
            [scope, name] if &**scope == "params" => tree::get(&self.options.params, &[name]),
            _ => None,
        }
    }

    /// Evaluates a value in the block `prefix` with the file's current params. In the
    /// `process` scope, closures may read the variables of a task.
    fn value(
        &mut self,
        file: &File,
        value: &Expression,
        prefix: &[Rc<str>],
        process_scope: bool,
    ) -> Result<Value, ConfigError> {
        let host: &dyn Host = &*self.host;
        let lookup =
            |name: &str, in_closure: bool| legacy_name(file, prefix, host, name, in_closure);
        let names = ConfigNames {
            functions: file.functions.clone(),
            inherited: file.inherited.clone(),
            lookup: Some(&lookup),
        };
        let code = lower_legacy_config_value(value, &file.source, process_scope, &names)
            .map_err(|errors| located(&file.path, &file.source, &errors))?;
        self.run(file, code)
    }

    /// Runs a statement of legacy syntax, like `System.err.println('...')`.
    fn statement(
        &mut self,
        file: &File,
        statement: &Statement,
        prefix: &[Rc<str>],
    ) -> Result<(), ConfigError> {
        let host: &dyn Host = &*self.host;
        let lookup =
            |name: &str, in_closure: bool| legacy_name(file, prefix, host, name, in_closure);
        let names = ConfigNames {
            functions: file.functions.clone(),
            inherited: file.inherited.clone(),
            lookup: Some(&lookup),
        };
        let code = lower_config_statement(statement, &file.source, &names)
            .map_err(|errors| located(&file.path, &file.source, &errors))?;
        self.run(file, code).map(|_| ())
    }

    /// Runs lowered code of the file.
    fn run(&mut self, file: &File, code: Code) -> Result<Value, ConfigError> {
        let mut interpreter = Interpreter::new(&mut *self.host);
        let params = tree::get(&file.tree, &["params"]).unwrap_or_else(tree::map);
        interpreter.set_global("params", params);
        let directory = |dir: &str| Value::Path(Rc::new(PathValue::parse(dir, "/")));
        let project = directory(&self.options.project_dir);
        interpreter.set_global("projectDir", project.clone());
        interpreter.set_global("baseDir", project);
        interpreter.set_global("launchDir", directory(&self.options.launch_dir));
        interpreter.set_global("secrets", tree::map());
        interpreter
            .evaluate(&Rc::new(code), &file.module)
            .map_err(|error| ConfigError::Failed {
                file: file.path.clone(),
                error,
            })
    }
}

/// Whether settings below `path` belong to the `process` scope.
/// The setting path of a target in an assignment chain, like `b` or `a.b` in `x = a.b = 5`.
fn chained_path(target: &Expression) -> Option<Vec<Name>> {
    match &target.kind {
        ExpressionKind::Variable(name) => Some(vec![Name {
            text: name.clone(),
            span: target.span,
        }]),
        ExpressionKind::Property {
            target: inner,
            name,
            access: Access::Normal,
        } => {
            let mut path = chained_path(inner)?;
            path.push(name.clone());
            Some(path)
        }
        _ => None,
    }
}

fn process_scope(path: &[Rc<str>]) -> bool {
    path.first().is_some_and(|scope| &**scope == "process")
}

/// The value of a name that strict syntax does not know, as the legacy parser resolves it
/// (decision 14): a local variable of the file, else, outside closures, a setting of the
/// block `prefix` or of an enclosing block, else an environment variable.
fn legacy_name(
    file: &File,
    prefix: &[Rc<str>],
    host: &dyn Host,
    name: &str,
    in_closure: bool,
) -> Option<Value> {
    if let Some(value) = file.variable(name) {
        return Some(value);
    }
    if in_closure {
        return None;
    }
    let setting = (0..=prefix.len()).rev().find_map(|depth| {
        let mut path: Vec<&str> = prefix[..depth].iter().map(|scope| &**scope).collect();
        path.push(name);
        tree::get(&file.tree, &path)
    });
    setting.or_else(|| host.environment(name).map(Value::string))
}

/// The exception that a `catch` clause sees for an error, if it can catch it: a file that
/// cannot be read or parsed is an `IOException` or a Groovy compilation error.
fn exception(error: &ConfigError) -> Option<Rc<Exception>> {
    if error.unsupported() {
        return None;
    }
    match error {
        ConfigError::Failed { error, .. } => thrown(error),
        ConfigError::Invalid { messages } => {
            let message = messages.join("\n");
            let class = match message.contains("Config file does not exist") {
                true => "NoSuchFileException",
                false => "GroovyRuntimeException",
            };
            Some(Rc::new(Exception::new(class, message)))
        }
    }
}

fn thrown(error: &Error) -> Option<Rc<Exception>> {
    match error {
        Error::Thrown { exception, .. } => Some(exception.clone()),
        Error::Located { error, .. } => thrown(error),
        Error::Unsupported { .. } => None,
    }
}

/// Errors of a file with their positions.
fn located(path: &Path, source: &str, errors: &[Diagnostic]) -> ConfigError {
    let lines = LineIndex::new(source);
    let messages = errors
        .iter()
        .map(|error| format!("{}:{}", path.display(), lines.locate(error)))
        .collect();
    ConfigError::Invalid { messages }
}

/// A command-line param as the config sees it: a string becomes the type of the config value
/// when it can, so `--n 7` is the Integer 7 for `params.n = 5`.
fn convert(cli: &Value, config: &Value) -> Value {
    let Value::String(text) = cli else {
        return cli.clone();
    };
    match config {
        Value::Boolean(_) if text.eq_ignore_ascii_case("true") => Value::Boolean(true),
        Value::Boolean(_) if text.eq_ignore_ascii_case("false") => Value::Boolean(false),
        Value::Number(_) => {
            if let Ok(value) = text.parse::<i32>() {
                Value::Number(Number::Integer(value))
            } else if let Ok(value) = text.parse::<i64>() {
                Value::Number(Number::Long(value))
            } else if let Ok(value) = text.parse::<f32>() {
                Value::Number(Number::Float(value))
            } else {
                cli.clone()
            }
        }
        _ => cli.clone(),
    }
}

fn invalid(file: &File, message: &str, span: Span) -> ConfigError {
    let lines = LineIndex::new(&file.source);
    let diagnostic = nextflow_parser::Diagnostic::new(message, span);
    ConfigError::Invalid {
        messages: vec![format!(
            "{}:{}",
            file.path.display(),
            lines.locate(&diagnostic)
        )],
    }
}

fn failed(file: &File, exception: Exception, span: Span) -> ConfigError {
    ConfigError::Failed {
        file: file.path.clone(),
        error: Error::Thrown {
            exception: Rc::new(exception),
            span,
        },
    }
}
