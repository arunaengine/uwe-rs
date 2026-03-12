//! Evaluating a process for one task: binding its inputs, its directives, then `when:`, the
//! script and the output patterns with the final `task` object.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::rc::Rc;

use nextflow_parser::Span;
use nextflow_parser::ast::BodyKind;

use super::statement::Completion;
use super::{Error, Frame, Interpreter, thrown, unsupported};
use crate::exception::Exception;
use crate::lowered::{Input, Module, OutputPart, Place, Process};
use crate::value::{List, ListKind, PathValue, Value};

/// The frame of one task, with the files its inputs stage.
pub struct TaskFrame {
    frame: Frame,
    /// Each input file and the name it is staged under.
    pub staged: Vec<(PathValue, String)>,
    /// The inputs and section variables by name, for config closures.
    variables: Vec<(Rc<str>, Rc<RefCell<Value>>)>,
}

/// A directive of one task, with its evaluated arguments.
#[derive(Debug)]
pub struct TaskDirective {
    pub name: Rc<str>,
    pub arguments: Vec<Value>,
}

/// What the evaluator decides about one task.
#[derive(Debug)]
pub struct RenderedTask {
    /// Whether `when:` lets the task run.
    pub run: bool,
    /// The task script, normalised as Nextflow writes `.command.sh`.
    pub script: String,
    pub outputs: Vec<RenderedOutput>,
}

#[derive(Debug)]
pub struct RenderedOutput {
    pub parts: Vec<RenderedPart>,
    pub tuple: bool,
    pub emit: Option<Rc<str>>,
    pub topic: Option<Rc<str>>,
    pub optional: bool,
}

#[derive(Debug)]
pub enum RenderedPart {
    Value(Value),
    /// A glob pattern of output files, with options like `arity` as a map.
    Path {
        pattern: String,
        options: Value,
    },
    Env(Rc<str>),
    /// A command whose output becomes the value, run after the script.
    Eval(String),
    Stdout,
}

impl Interpreter<'_> {
    /// Binds the values of one task to the process inputs, in their order. Input files become
    /// staged paths, which print their staged name.
    pub fn bind_task(
        &mut self,
        module: &Rc<Module>,
        process: &Process,
        inputs: &[Value],
    ) -> Result<TaskFrame, Error> {
        let slots: Vec<_> = (0..process.slot_count)
            .map(|_| Rc::new(RefCell::new(Value::Null)))
            .collect();
        let variables = process
            .variables
            .iter()
            .map(|(name, slot)| (name.clone(), slots[*slot].clone()))
            .collect();
        let mut task = TaskFrame {
            frame: Frame {
                module: module.clone(),
                slots,
                captured: Vec::new(),
            },
            staged: Vec::new(),
            variables,
        };
        if inputs.len() != process.inputs.len() {
            let message = format!(
                "Process `{}` declares {} inputs, but got {}",
                process.name,
                process.inputs.len(),
                inputs.len()
            );
            return Err(unsupported(message, process.span));
        }
        for (input, value) in process.inputs.iter().zip(inputs) {
            self.bind_input(&mut task, input, value, process.span)?;
        }
        Ok(task)
    }

    fn bind_input(
        &mut self,
        task: &mut TaskFrame,
        input: &Input,
        value: &Value,
        span: Span,
    ) -> Result<(), Error> {
        match input {
            Input::Value { slot } => task.frame.set(Place::Local(*slot), value.clone()),
            Input::Each(inner) => self.bind_input(task, inner, value, span)?,
            Input::Tuple(parts) => {
                let items = match value {
                    Value::List(list) => list.borrow().items.clone(),
                    other => vec![other.clone()],
                };
                if items.len() != parts.len() {
                    let message = format!(
                        "Input tuple does not match tuple declaration in process: {} values for {} parts",
                        items.len(),
                        parts.len()
                    );
                    return Err(unsupported(message, span));
                }
                for (part, item) in parts.iter().zip(&items) {
                    self.bind_input(task, part, item, span)?;
                }
            }
            Input::Path {
                slot,
                pattern,
                options,
            } => {
                let options = self.arguments(&mut task.frame, options)?;
                let stage_as = match options.first() {
                    Some(Value::Map(map)) => {
                        let map = map.borrow();
                        let key = |name: &str| {
                            map.entries
                                .get(&crate::value::Key(Value::string(name)))
                                .cloned()
                        };
                        key("stageAs").or_else(|| key("name"))
                    }
                    _ => None,
                };
                let name = match (pattern, stage_as) {
                    (Some(pattern), _) => {
                        Some(self.expression(&mut task.frame, pattern)?.to_string())
                    }
                    (None, Some(name)) => Some(name.to_string()),
                    (None, None) => None,
                };
                let staged = stage(task, value, name.as_deref(), span)?;
                if let Some(slot) = slot {
                    task.frame.set(Place::Local(*slot), staged);
                }
            }
            Input::Env(_) | Input::Stdin => {}
        }
        Ok(())
    }

    /// The directives of the process in source order, with their evaluated arguments.
    pub fn directives(
        &mut self,
        task: &mut TaskFrame,
        process: &Process,
        task_object: &Value,
    ) -> Result<Vec<TaskDirective>, Error> {
        let variables = task.variables.clone();
        self.with_task(variables, task_object, |this| {
            let mut directives = Vec::new();
            for directive in &process.directives {
                let arguments = this.arguments(&mut task.frame, &directive.arguments)?;
                directives.push(TaskDirective {
                    name: directive.name.clone(),
                    arguments,
                });
            }
            Ok(directives)
        })
    }

    /// The value of a config setting for a task: a closure is called with the task's
    /// variables in scope, any other value stays as it is.
    pub fn resolve_setting(
        &mut self,
        task: &TaskFrame,
        value: &Value,
        task_object: &Value,
    ) -> Result<Value, Error> {
        match value {
            Value::Closure(closure) => {
                self.with_task(task.variables.clone(), task_object, |this| {
                    this.call_closure_value(closure, Vec::new(), Span::default())
                })
            }
            other => Ok(other.clone()),
        }
    }

    /// Runs `f` with `task` and the task's variables in scope.
    fn with_task<T>(
        &mut self,
        variables: Vec<(Rc<str>, Rc<RefCell<Value>>)>,
        task_object: &Value,
        f: impl FnOnce(&mut Self) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let previous = self.globals.insert("task".into(), task_object.clone());
        let previous_variables = std::mem::replace(&mut self.task_variables, variables);
        let result = f(self);
        self.task_variables = previous_variables;
        match previous {
            Some(value) => self.globals.insert("task".into(), value),
            None => self.globals.remove("task"),
        };
        result
    }

    /// A name of a config closure, looked up as Nextflow's task context does: the task's
    /// variables, the script's globals like `params`, then the launch environment.
    pub(super) fn dynamic(&mut self, name: &str, span: Span) -> Result<Value, Error> {
        if let Some((_, cell)) = self.task_variables.iter().find(|(n, _)| **n == *name) {
            return Ok(cell.borrow().clone());
        }
        if let Some(value) = self.globals.get(name) {
            return Ok(value.clone());
        }
        if let Some(value) = self.host.environment(name) {
            return Ok(Value::string(value));
        }
        let message = format!("No such variable: {name}");
        Err(thrown(
            Exception::new("MissingPropertyException", message),
            span,
        ))
    }

    /// Evaluates `when:`, the script (or the stub) and the output patterns of a task.
    pub fn render(
        &mut self,
        task: &mut TaskFrame,
        process: &Process,
        task_object: &Value,
        stub: bool,
    ) -> Result<RenderedTask, Error> {
        let variables = task.variables.clone();
        self.with_task(variables, task_object, |this| {
            this.render_sections(task, process, task_object, stub)
        })
    }

    fn render_sections(
        &mut self,
        task: &mut TaskFrame,
        process: &Process,
        task_object: &Value,
        stub: bool,
    ) -> Result<RenderedTask, Error> {
        let run = match &process.when {
            Some(when) => self.expression(&mut task.frame, when)?.truth(),
            None => true,
        };
        let statements = match (&process.stub, &process.body) {
            (Some(statements), _) if stub => statements,
            (_, Some(body)) if matches!(body.kind, BodyKind::Script | BodyKind::Implicit) => {
                &body.statements
            }
            (_, Some(body)) => {
                let kind = if body.kind == BodyKind::Shell {
                    "shell:"
                } else {
                    "exec:"
                };
                return Err(unsupported(format!("`{kind}` sections"), process.span));
            }
            (_, None) => return Err(unsupported("processes without a script", process.span)),
        };
        let script = match self.block(&mut task.frame, statements)? {
            Completion::Normal(value) | Completion::Return(value) => value.to_java_string(),
        };
        let shell = match task_object {
            Value::Map(map) => map
                .borrow()
                .entries
                .get(&crate::value::Key(Value::string("shell")))
                .cloned(),
            _ => None,
        };
        let mut outputs = Vec::new();
        for output in &process.outputs {
            let mut parts = Vec::new();
            for part in &output.parts {
                parts.push(match part {
                    OutputPart::Value(value) => {
                        RenderedPart::Value(self.expression(&mut task.frame, value)?)
                    }
                    OutputPart::Path { pattern, options } => RenderedPart::Path {
                        pattern: self.expression(&mut task.frame, pattern)?.to_string(),
                        options: self
                            .arguments(&mut task.frame, options)?
                            .into_iter()
                            .next()
                            .unwrap_or(Value::map([])),
                    },
                    OutputPart::Env(name) => RenderedPart::Env(name.clone()),
                    OutputPart::Eval(command) => {
                        RenderedPart::Eval(self.expression(&mut task.frame, command)?.to_string())
                    }
                    OutputPart::Stdout => RenderedPart::Stdout,
                });
            }
            let optional = match &output.optional {
                Some(optional) => self.expression(&mut task.frame, optional)?.truth(),
                None => false,
            };
            outputs.push(RenderedOutput {
                parts,
                tuple: output.tuple,
                emit: output.emit.clone(),
                topic: output.topic.clone(),
                optional,
            });
        }
        Ok(RenderedTask {
            run,
            script: normalise_script(&script, shell.as_ref()),
            outputs,
        })
    }
}

/// The staged form of an input value: a path, or a `BlankSeparatedList` of paths. A name
/// pattern names the files as Nextflow's `expandWildcards` does.
fn stage(
    task: &mut TaskFrame,
    value: &Value,
    name: Option<&str>,
    span: Span,
) -> Result<Value, Error> {
    let files = match value {
        Value::List(list) => list
            .borrow()
            .items
            .iter()
            .map(|item| input_path(item, span))
            .collect::<Result<Vec<_>, _>>()?,
        other => vec![input_path(other, span)?],
    };
    let size = files.len();
    let mut pattern = name
        .filter(|name| !name.is_empty() && *name != "*")
        .map(str::to_owned);
    if let Some(pattern) = &mut pattern
        && !pattern.contains(['*', '?'])
        && size > 1
    {
        pattern.push('*');
    }
    let mut staged = Vec::new();
    for (index, path) in files.into_iter().enumerate() {
        let name = match &pattern {
            Some(pattern) => stage_name(pattern, path.name(), index + 1, size),
            None => path.name().to_owned(),
        };
        task.staged.push((path.clone(), name.clone()));
        staged.push(Value::Path(Rc::new(PathValue {
            staged: Some(name),
            ..path
        })));
    }
    match value {
        Value::List(_) => {
            let list = List::new(ListKind::BlankSeparated, staged);
            Ok(Value::List(Rc::new(RefCell::new(list))))
        }
        _ => Ok(staged.remove(0)),
    }
}

/// The name of the `index`-th of `size` files for a name pattern: `*` is the index, or
/// nothing in the file name of a single file; `?` is the index padded to its width; a file
/// name of `*` is the file's own name.
fn stage_name(pattern: &str, own: &str, index: usize, size: usize) -> String {
    let (parent, name) = match pattern.rsplit_once('/') {
        Some((parent, name)) => (Some(parent), name),
        None => (None, pattern),
    };
    let expand = |text: &str, strip: bool| {
        let number = index.to_string();
        let starred = text.replace('*', if strip { "" } else { &number });
        let mut result = String::new();
        let mut characters = starred.chars().peekable();
        while let Some(character) = characters.next() {
            if character == '?' {
                let mut width = 1;
                while characters.next_if_eq(&'?').is_some() {
                    width += 1;
                }
                result.push_str(&format!("{index:0width$}"));
            } else {
                result.push(character);
            }
        }
        result
    };
    let name = if name.is_empty() || name == "*" {
        own.to_owned()
    } else {
        expand(name, size <= 1)
    };
    match parent {
        Some(parent) => format!("{}/{name}", expand(parent, false)),
        None => name,
    }
}

/// The path of an input value: a path, or text that is an absolute path or a URL, as
/// Nextflow's `normalizeInputToFile` takes it.
fn input_path(value: &Value, span: Span) -> Result<PathValue, Error> {
    match value {
        Value::Path(path) => Ok((**path).clone()),
        Value::String(text) | Value::GString(text)
            if text.starts_with('/') || text.contains("://") =>
        {
            Ok(PathValue::parse(text, "/"))
        }
        other => Err(not_a_file(other, span)),
    }
}

fn not_a_file(value: &Value, span: Span) -> Error {
    let message = format!(
        "Not a valid path value type: {} ({value})",
        value.java_class_name()
    );
    thrown(Exception::illegal_argument(message), span)
}

/// Nextflow's `normalizeScript`: Groovy's `stripIndent`, `trim`, a final newline, and the
/// shebang of the `shell` setting unless the script starts with one.
pub fn normalise_script(script: &str, shell: Option<&Value>) -> String {
    let mut result = strip_indent(script)
        .trim_matches(|c: char| c <= ' ')
        .to_owned();
    result.push('\n');
    if !result.starts_with("#!") {
        let shell = match shell {
            Some(Value::List(list)) => list
                .borrow()
                .items
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join(" "),
            Some(Value::String(text)) => text.to_string(),
            _ => "/bin/bash -ue".to_owned(),
        };
        let shebang = if shell.starts_with('/') {
            format!("#!{shell}")
        } else {
            format!("#!/usr/bin/env {shell}")
        };
        result.insert_str(0, &format!("{shebang}\n"));
    }
    result
}

/// Groovy's `stripIndent()`: the smallest indent of the lines that are not blank is removed;
/// blank lines become empty. Nothing changes when that indent is zero.
fn strip_indent(text: &str) -> String {
    let lines = java_lines(text);
    let indent = lines
        .iter()
        .filter(|line| !line.chars().all(java_whitespace))
        .map(|line| line.chars().take_while(|c| java_whitespace(*c)).count())
        .min()
        .unwrap_or(0);
    if text.is_empty() || indent == 0 {
        return text.to_owned();
    }
    let mut result = String::new();
    for line in &lines {
        if !line.chars().all(java_whitespace) {
            result.extend(line.chars().skip(indent));
        }
        result.push('\n');
    }
    if !text.ends_with('\n') {
        result.pop();
    }
    result
}

/// Lines as Java's `BufferedReader` reads them: split on `\n`, `\r\n` or `\r`, without an
/// empty line after a final line break.
fn java_lines(text: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\n' | '\r' => {
                if character == '\r' && characters.peek() == Some(&'\n') {
                    characters.next();
                }
                lines.push(std::mem::take(&mut current));
            }
            other => current.push(other),
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// Java's `Character.isWhitespace`, which excludes the non-breaking spaces.
fn java_whitespace(c: char) -> bool {
    matches!(c, '\u{1C}'..='\u{1F}')
        || (c.is_whitespace() && !matches!(c, '\u{A0}' | '\u{85}' | '\u{2007}' | '\u{202F}'))
}
