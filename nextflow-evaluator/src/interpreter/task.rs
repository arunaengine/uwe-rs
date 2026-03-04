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
        let mut task = TaskFrame {
            frame: Frame {
                module: module.clone(),
                slots: (0..process.slot_count)
                    .map(|_| Rc::new(RefCell::new(Value::Null)))
                    .collect(),
                captured: Vec::new(),
            },
            staged: Vec::new(),
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
                if name
                    .as_deref()
                    .is_some_and(|name| name.contains(['*', '?']))
                {
                    return Err(unsupported(
                        "staging input files with a wildcard name",
                        span,
                    ));
                }
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
        let previous = self.globals.insert("task".into(), task_object.clone());
        let mut directives = Vec::new();
        let result = process.directives.iter().try_for_each(|directive| {
            let arguments = self.arguments(&mut task.frame, &directive.arguments)?;
            directives.push(TaskDirective {
                name: directive.name.clone(),
                arguments,
            });
            Ok(())
        });
        self.restore_task(previous);
        result.map(|()| directives)
    }

    /// Evaluates `when:`, the script (or the stub) and the output patterns of a task.
    pub fn render(
        &mut self,
        task: &mut TaskFrame,
        process: &Process,
        task_object: &Value,
        stub: bool,
    ) -> Result<RenderedTask, Error> {
        let previous = self.globals.insert("task".into(), task_object.clone());
        let result = self.render_sections(task, process, task_object, stub);
        self.restore_task(previous);
        result
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

    fn restore_task(&mut self, previous: Option<Value>) {
        match previous {
            Some(value) => self.globals.insert("task".into(), value),
            None => self.globals.remove("task"),
        };
    }
}

/// The staged form of an input value: a path, or a `BlankSeparatedList` of paths.
fn stage(
    task: &mut TaskFrame,
    value: &Value,
    name: Option<&str>,
    span: Span,
) -> Result<Value, Error> {
    let mut stage_one = |path: &PathValue| {
        let staged = name.unwrap_or(path.name()).to_owned();
        task.staged.push((path.clone(), staged.clone()));
        Value::Path(Rc::new(PathValue {
            staged: Some(staged),
            ..path.clone()
        }))
    };
    match value {
        Value::Path(path) => Ok(stage_one(path)),
        Value::List(list) => {
            let mut staged = Vec::new();
            for item in list.borrow().items.iter() {
                match item {
                    Value::Path(path) => staged.push(stage_one(path)),
                    other => return Err(not_a_file(other, span)),
                }
            }
            let list = List::new(ListKind::BlankSeparated, staged);
            Ok(Value::List(Rc::new(RefCell::new(list))))
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
