//! Evaluating one config file: its statements run from top to bottom into a tree.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::{Error, Host, Interpreter};
use nextflow_evaluator::lower::lower_config_value;
use nextflow_evaluator::lowered::Module;
use nextflow_evaluator::number::Number;
use nextflow_evaluator::value::{PathValue, Value};
use nextflow_parser::ast::{ConfigStatement, Expression, Literal};
use nextflow_parser::{LineIndex, Span, parse_config};

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
        self.file(path, params)
    }

    /// Evaluates a file whose `params` start as `params`, and returns its tree.
    fn file(&mut self, path: &Path, params: Value) -> Result<Value, ConfigError> {
        let source = self.source(path)?;
        let parsed = parse_config(&source);
        if !parsed.errors.is_empty() {
            let lines = LineIndex::new(&source);
            let messages = parsed
                .errors
                .iter()
                .map(|error| format!("{}:{}", path.display(), lines.locate(error)))
                .collect();
            return Err(ConfigError::Invalid { messages });
        }
        let tree = tree::map();
        tree::assign(&tree, &["params".into()], params).expect("a new tree");
        let mut file = File {
            path: path.to_owned(),
            source,
            tree,
            module: Rc::new(Module::default()),
        };
        *file.module.source.borrow_mut() = Some(file.source.clone());
        self.statements(&mut file, &parsed.tree.statements, &[], false)?;
        Ok(file.tree)
    }

    fn source(&mut self, path: &Path) -> Result<Rc<str>, ConfigError> {
        if let Some(source) = self.sources.get(path) {
            return Ok(source.clone());
        }
        let source: Rc<str> = std::fs::read_to_string(path)
            .map_err(|error| ConfigError::Invalid {
                messages: vec![format!(
                    "Config file does not exist: {} ({error})",
                    path.display()
                )],
            })?
            .into();
        self.sources.insert(path.to_owned(), source.clone());
        Ok(source)
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
                ConfigStatement::Include { path, span } => {
                    if !prefix.is_empty() {
                        let message =
                            "Config includes are only allowed at the top-level or in a profile";
                        return Err(invalid(file, message, *span));
                    }
                    self.include(file, path)?;
                }
                ConfigStatement::Assign { path, value, span } => {
                    let mut full: Vec<Rc<str>> = prefix.to_vec();
                    full.extend(path.iter().map(|name| Rc::from(name.text.as_str())));
                    let value = match self.cli_param(&full) {
                        Some(cli) => {
                            let config = self.value(file, value, &full)?;
                            convert(&cli, &config)
                        }
                        None => self.value(file, value, &full)?,
                    };
                    tree::assign(&file.tree, &full, value)
                        .map_err(|message| invalid(file, &message, *span))?;
                }
                ConfigStatement::Block { name, body, .. }
                    if name.text == "profiles" && prefix.is_empty() =>
                {
                    self.profiles(file, body)?;
                }
                ConfigStatement::Block { name, body, .. } => {
                    let mut inner = prefix.to_vec();
                    inner.push(name.text.as_str().into());
                    self.statements(file, body, &inner, false)?;
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
                    if prefix.len() != 1 || &*prefix[0] != "process" {
                        let message = "Config selectors are only allowed in the `process` scope";
                        return Err(invalid(file, message, *span));
                    }
                    if kind.text != "withName" && kind.text != "withLabel" {
                        let message = format!("Unknown config selector `{}`", kind.text);
                        return Err(invalid(file, &message, kind.span));
                    }
                    let scope: Rc<str> = format!("{}:{}", kind.text, target.text).into();
                    self.statements(file, body, &["process".into(), scope], false)?;
                }
            }
        }
        Ok(())
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
                ConfigStatement::Assign { span, .. }
                | ConfigStatement::Include { span, .. }
                | ConfigStatement::Apply { span, .. }
                | ConfigStatement::Selector { span, .. } => {
                    let message = "Only profile blocks are allowed in the `profiles` scope";
                    return Err(invalid(file, message, *span));
                }
            }
        }
        let standard = vec!["standard".to_owned()];
        let selected = self.options.profiles.clone().unwrap_or(standard);
        for name in &selected {
            if let Some(body) = defined.get(name.as_str()) {
                self.statements(file, body, &[], true)?;
            }
        }
        Ok(())
    }

    /// `includeConfig path`: the file runs with a copy of the current params, and its tree
    /// merges into this one.
    fn include(&mut self, file: &mut File, path: &Expression) -> Result<(), ConfigError> {
        let target = self.value(file, path, &[])?;
        let text = target.to_string();
        if !target.truth() {
            let exception =
                Exception::assertion(format!("assert includeFile\n       |\n       {target}"));
            return Err(failed(file, exception, path.span));
        }
        if text == "/dev/null" {
            return Ok(());
        }
        if text.contains("://") {
            return Err(ConfigError::Failed {
                file: file.path.clone(),
                error: Error::Unsupported {
                    message: format!("including remote config files like {text}"),
                    span: path.span,
                },
            });
        }
        let base = file.path.parent().unwrap_or(Path::new("/"));
        let included = base.join(&text);
        let params = tree::get(&file.tree, &["params"]).unwrap_or_else(tree::map);
        let tree = self.file(&included, tree::deep_copy(&params))?;
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

    /// Evaluates a setting's value with the file's current params.
    fn value(
        &mut self,
        file: &File,
        value: &Expression,
        path: &[Rc<str>],
    ) -> Result<Value, ConfigError> {
        let process_scope = path.first().is_some_and(|scope| &**scope == "process");
        let code = lower_config_value(value, &file.source, process_scope).map_err(|errors| {
            let lines = LineIndex::new(&file.source);
            ConfigError::Invalid {
                messages: errors
                    .iter()
                    .map(|error| format!("{}:{}", file.path.display(), lines.locate(error)))
                    .collect(),
            }
        })?;
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
