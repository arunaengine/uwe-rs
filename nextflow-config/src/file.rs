//! Evaluating one config file: its statements run from top to bottom into a tree.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use nextflow_evaluator::interpreter::{Error, Host, Interpreter};
use nextflow_evaluator::lower::lower_config_value;
use nextflow_evaluator::lowered::Module;
use nextflow_evaluator::value::{PathValue, Value};
use nextflow_parser::ast::{ConfigStatement, Expression};
use nextflow_parser::{LineIndex, Span, parse_config};

use crate::tree;
use crate::{ConfigError, Options};

pub(crate) struct Evaluator<'a> {
    options: &'a Options,
    host: &'a mut dyn Host,
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
        self.statements(&mut file, &parsed.tree.statements, &[])?;
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

    /// Runs statements below the scope `prefix`.
    fn statements(
        &mut self,
        file: &mut File,
        statements: &[ConfigStatement],
        prefix: &[Rc<str>],
    ) -> Result<(), ConfigError> {
        for statement in statements {
            match statement {
                ConfigStatement::Include { path, span } => {
                    if !prefix.is_empty() {
                        let message =
                            "Config includes are only allowed at the top-level or in a profile";
                        return Err(invalid(file, message, *span));
                    }
                    let error = Error::Unsupported {
                        message: "`includeConfig`".into(),
                        span: path.span,
                    };
                    return Err(ConfigError::Failed {
                        file: file.path.clone(),
                        error,
                    });
                }
                ConfigStatement::Assign { path, value, span } => {
                    let mut full: Vec<Rc<str>> = prefix.to_vec();
                    full.extend(path.iter().map(|name| Rc::from(name.text.as_str())));
                    let value = self.value(file, value, &full)?;
                    tree::assign(&file.tree, &full, value)
                        .map_err(|message| invalid(file, &message, *span))?;
                }
                ConfigStatement::Block { name, body, .. } => {
                    let mut inner = prefix.to_vec();
                    inner.push(name.text.as_str().into());
                    self.statements(file, body, &inner)?;
                }
                ConfigStatement::Apply { span, .. } => {
                    let error = Error::Unsupported {
                        message: "`plugins` blocks".into(),
                        span: *span,
                    };
                    return Err(ConfigError::Failed {
                        file: file.path.clone(),
                        error,
                    });
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
                    self.statements(file, body, &["process".into(), scope])?;
                }
            }
        }
        Ok(())
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
