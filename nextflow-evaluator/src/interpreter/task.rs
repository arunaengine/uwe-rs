//! Evaluating a process for one task: binding its inputs, its directives, then `when:`, the
//! script and the output patterns with the final `task` object.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::rc::Rc;

use md5::Digest;
use nextflow_parser::Span;
use nextflow_parser::ast::BodyKind;

use super::{Error, Frame, Interpreter, thrown, unsupported};
use crate::exception::Exception;
use crate::lowered::{Expression, ExpressionKind, Input, Module, OutputPart, Place, Process};
use crate::value::{List, ListKind, PathValue, Value};

/// The frame of one task, with the files its inputs stage.
pub struct TaskFrame {
    frame: Frame,
    /// Each input file and the name it is staged under.
    pub staged: Vec<(PathValue, String)>,
    pub input_environment: Vec<(Rc<str>, Value)>,
    pub stdin: Option<Value>,
    /// The inputs and section variables by name, for config closures.
    variables: Vec<(Rc<str>, Rc<RefCell<Value>>)>,
    /// The name of the process, for messages.
    process: Rc<str>,
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
    /// The value of a typed output, which `Interpreter::typed_output` computes from the parts.
    pub typed: Option<Rc<crate::lowered::Expression>>,
    /// Whether the output only feeds its topic.
    pub hidden: bool,
}

#[derive(Debug)]
pub enum RenderedPart {
    Value(Value),
    /// The glob patterns of output files, with options like `arity` as a map. A closure or a
    /// list gives several, as Nextflow's `FileOutParam` takes them.
    Path {
        patterns: Vec<String>,
        options: Value,
    },
    Env(Rc<str>),
    /// A command whose output becomes the value, run after the script.
    Eval(String),
    Stdout,
    /// The patterns of `file()` or `files()` of a typed output, with its options as a map.
    Files {
        patterns: Vec<String>,
        options: Value,
        single: bool,
    },
}

impl Interpreter<'_> {
    /// The value of a typed output after its task, from the values of its parts.
    pub fn typed_output(
        &mut self,
        task: &mut TaskFrame,
        value: &crate::lowered::Expression,
        parts: Vec<Value>,
    ) -> Result<Value, Error> {
        let outer = std::mem::replace(&mut self.task_outputs, parts);
        let result = self.expression(&mut task.frame, value);
        self.task_outputs = outer;
        result
    }

    /// Binds the values of one task to the process inputs, in their order. Input files become
    /// staged paths, which print their staged name.
    pub fn bind_task(
        &mut self,
        module: &Rc<Module>,
        process: &Process,
        inputs: &[Value],
    ) -> Result<TaskFrame, Error> {
        self.bind_task_as(module, process, &process.name, inputs)
    }

    /// Like `bind_task`, with the name that messages give the process, like its fully
    /// qualified name `WORKFLOW:PROCESS`.
    pub fn bind_task_as(
        &mut self,
        module: &Rc<Module>,
        process: &Process,
        name: &str,
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
                unset: RefCell::default(),
            },
            staged: Vec::new(),
            input_environment: Vec::new(),
            stdin: None,
            variables,
            process: name.into(),
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
        // A typed input may only be null when its type allows it.
        if let Some(index) =
            (process.non_null.iter()).find(|index| matches!(inputs.get(**index), Some(Value::Null)))
        {
            let message = format!(
                "[{name}] input at index {index} cannot be null -- append `?` to the type \
                 annotation to mark it as nullable"
            );
            return Err(thrown(
                Exception::new("ScriptRuntimeException", message),
                process.span,
            ));
        }
        // Values come first, as the names of staged files may use any of them, like
        // `path("${dir}/*")` with a later `val dir`, which the task's variables give.
        let outer = std::mem::replace(&mut self.task_variables, task.variables.clone());
        let mut bound = Ok(());
        for paths in [false, true] {
            for (input, value) in process.inputs.iter().zip(inputs) {
                if bound.is_ok() {
                    bound = self.bind_input(&mut task, input, value, paths, process.span);
                }
            }
        }
        // The `stage:` section of a typed process sets variables and standard input from them.
        if bound.is_ok() {
            for (name, value) in &process.stage_env {
                match self.expression(&mut task.frame, value) {
                    Ok(value) => task.input_environment.push((name.clone(), value)),
                    Err(error) => bound = Err(error),
                }
            }
            if let Some(value) = &process.stage_stdin {
                match self.expression(&mut task.frame, value) {
                    Ok(value) => task.stdin = Some(value),
                    Err(error) => bound = Err(error),
                }
            }
        }
        self.task_variables = outer;
        bound.map(|()| task)
    }

    /// Binds one input: its values, or with `paths` its files.
    fn bind_input(
        &mut self,
        task: &mut TaskFrame,
        input: &Input,
        value: &Value,
        paths: bool,
        span: Span,
    ) -> Result<(), Error> {
        match input {
            Input::Value { slot } if !paths => task.frame.set(Place::Local(*slot), value.clone()),
            Input::Value { .. } => {}
            Input::Each(inner) => self.bind_input(task, inner, value, paths, span)?,
            Input::Tuple(parts) => {
                let items = match value {
                    Value::List(list) => list.borrow().items.clone(),
                    other => vec![other.clone()],
                };
                // Nextflow's strict syntax makes this an error; legacy syntax only warns.
                if items.len() != parts.len() {
                    let message = format!(
                        "Input tuple does not match tuple declaration in process `{}` -- offending value: {value}",
                        task.process
                    );
                    let exception = Exception::new("ProcessUnrecoverableException", message);
                    return Err(thrown(exception, span));
                }
                for (part, item) in parts.iter().zip(&items) {
                    self.bind_input(task, part, item, paths, span)?;
                }
            }
            // A record binds its fields by name; a missing field is null.
            Input::Record(fields) => {
                for (name, field) in fields {
                    let item = match value {
                        Value::Map(map) => (map.borrow().entries)
                            .get(&crate::value::Key(Value::String(name.clone())))
                            .cloned()
                            .unwrap_or(Value::Null),
                        _ => Value::Null,
                    };
                    self.bind_input(task, field, &item, paths, span)?;
                }
            }
            Input::Path { .. } if !paths => {}
            Input::Path {
                slot,
                pattern,
                options,
                coerce,
                listed,
            } => {
                let options = self.arguments(&mut task.frame, options)?;
                let option = |name: &str| match options.first() {
                    Some(Value::Map(map)) => map
                        .borrow()
                        .entries
                        .get(&crate::value::Key(Value::string(name)))
                        .cloned(),
                    _ => None,
                };
                let stage_as = option("stageAs").or_else(|| option("name"));
                // One file is a path, not a list, unless the arity allows more.
                let single = option("arity").is_none_or(|arity| {
                    let arity = arity.to_string();
                    let most = arity
                        .rsplit("..")
                        .next()
                        .unwrap_or(&arity)
                        .trim()
                        .to_owned();
                    most == "1"
                });
                let name = match (pattern, stage_as) {
                    (Some(pattern), _) => {
                        Some(self.expression(&mut task.frame, pattern)?.to_string())
                    }
                    (None, Some(name)) => Some(name.to_string()),
                    (None, None) => None,
                };
                let value = match (coerce, value) {
                    // A set of files, as a typed input takes it, stages as a list.
                    (true, Value::Set(_)) => Value::list(super::library::elements(value)),
                    (true, _) => value.clone(),
                    (false, _) => self.file_values(task, value)?,
                };
                let mut staged = stage(task, &value, name.as_deref(), single, span)?;
                if let (true, Value::List(list)) = (listed, &staged) {
                    let items = list.borrow().items.clone();
                    staged = Value::list(items);
                }
                if let Some(slot) = slot {
                    task.frame.set(Place::Local(*slot), staged);
                }
            }
            Input::Env(name) if !paths => {
                task.input_environment.push((name.clone(), value.clone()));
            }
            Input::Stdin if !paths => task.stdin = Some(value.clone()),
            Input::Env(_) | Input::Stdin => {}
        }
        Ok(())
    }

    /// Converts nonpath legacy file inputs into text files named `input.N` after prior task files.
    /// Files live under launch work/tmp in an MD5-named folder, matching normalizeInputToFile.
    fn file_values(&mut self, task: &TaskFrame, value: &Value) -> Result<Value, Error> {
        let items = match value {
            Value::List(list) => list.borrow().items.clone(),
            other => vec![other.clone()],
        };
        let mut count = task.staged.len();
        let mut files = Vec::new();
        for item in items {
            if let Value::Path(_) = item {
                files.push(item);
                continue;
            }
            count += 1;
            let text = match item {
                Value::Null => String::new(),
                other => other.to_string(),
            };
            let digest = md5::Md5::digest(text.as_bytes());
            let hash: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
            let launch = self.host().launch_dir();
            let path = PathValue::parse(&format!("work/tmp/{hash}/input.{count}"), &launch);
            self.host().write(&path, text.as_bytes())?;
            files.push(Value::Path(Rc::new(path)));
        }
        Ok(match value {
            Value::List(_) => Value::List(Rc::new(crate::value::ListCell::new(List::new(
                ListKind::ArrayList,
                files,
            )))),
            _ => files.remove(0),
        })
    }

    /// The directives of the process in source order, with their evaluated arguments.
    pub fn directives(
        &mut self,
        task: &mut TaskFrame,
        process: &Process,
        task_object: &Value,
    ) -> Result<Vec<TaskDirective>, Error> {
        let variables = task.variables.clone();
        let result = self.with_task(variables, task_object, |this| {
            this.block(&mut task.frame, &process.setup)?;
            let mut directives = Vec::new();
            for directive in &process.directives {
                if let Some(guard) = &directive.guard
                    && !this.expression(&mut task.frame, guard)?.truth()
                {
                    continue;
                }
                // A closure, like `tag { x }`, gives its value, as Nextflow's `TaskConfig`
                // calls it for the task.
                let resolved = this
                    .arguments(&mut task.frame, &directive.arguments)
                    .and_then(|arguments| {
                        arguments
                            .into_iter()
                            .map(|argument| match argument {
                                Value::Closure(closure) => {
                                    this.call_closure_value(&closure, Vec::new(), directive.span)
                                }
                                other => Ok(other),
                            })
                            .collect::<Result<Vec<Value>, Error>>()
                    });
                let arguments = match resolved {
                    Ok(arguments) => arguments,
                    // Nextflow reads the tag only to name the task, and ignores its errors.
                    Err(error) if &*directive.name == "tag" && error.exception().is_some() => {
                        this.error_module = None;
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                directives.push(TaskDirective {
                    name: directive.name.clone(),
                    arguments,
                });
            }
            Ok(directives)
        });
        self.in_script(&task.frame, result)
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

    /// Calls a closure of a directive for a task, like the `saveAs` of `publishDir`, with the
    /// task's variables and `task` in scope. Another value is its own result.
    pub fn call_in_task(
        &mut self,
        task: &TaskFrame,
        task_object: &Value,
        closure: &Value,
        arguments: Vec<Value>,
    ) -> Result<Value, Error> {
        let Value::Closure(closure) = closure else {
            return Ok(closure.clone());
        };
        self.with_task(task.variables.clone(), task_object, |this| {
            this.call_closure_value(closure, arguments, Span::default())
        })
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
        // Of two inputs with one name, which legacy code allows, the later one counts.
        let variable = self.task_variables.iter().rev().find(|(n, _)| **n == *name);
        if let Some((_, cell)) = variable {
            return Ok(cell.borrow().clone());
        }
        if let Some(value) = self.globals.get(name) {
            return Ok(value.clone());
        }
        if let Some(value) = self.host.environment(name) {
            return Ok(Value::string(value));
        }
        if let Some(delegate) = self.delegates.last().cloned() {
            return self.property(&delegate, name, span);
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
        if !self.task_guard(task, process, task_object)? {
            return Ok(RenderedTask {
                run: false,
                script: String::new(),
                outputs: Vec::new(),
            });
        }
        self.render_body(task, process, task_object, stub)
    }

    /// Evaluates the guard before the body or persistent output probe runs.
    pub fn task_guard(
        &mut self,
        task: &mut TaskFrame,
        process: &Process,
        task_object: &Value,
    ) -> Result<bool, Error> {
        let result = self.with_task(task.variables.clone(), task_object, |this| {
            match &process.when {
                Some(when) => Ok(this.expression(&mut task.frame, when)?.truth()),
                None => Ok(true),
            }
        });
        self.in_script(&task.frame, result)
    }

    /// Renders a task whose guard was already accepted.
    pub fn render_body(
        &mut self,
        task: &mut TaskFrame,
        process: &Process,
        task_object: &Value,
        stub: bool,
    ) -> Result<RenderedTask, Error> {
        let variables = task.variables.clone();
        let result = self.with_task(variables, task_object, |this| {
            this.render_sections(task, process, task_object, stub)
        });
        self.in_script(&task.frame, result)
    }

    /// Resolves declared outputs without running an exec body for stored-output reuse.
    pub fn render_probe(
        &mut self,
        task: &mut TaskFrame,
        process: &Process,
        task_object: &Value,
    ) -> Result<Option<RenderedTask>, Error> {
        fn safe(value: &Expression) -> bool {
            match &value.kind {
                ExpressionKind::Literal(_)
                | ExpressionKind::Variable(_)
                | ExpressionKind::Binding(_)
                | ExpressionKind::Builtin(_) => true,
                ExpressionKind::Interpolate { parts, .. } => parts.iter().all(|part| match part {
                    crate::lowered::Part::Text(_) => true,
                    crate::lowered::Part::Value(value) => safe(value),
                }),
                ExpressionKind::List(items) => items.iter().all(safe),
                ExpressionKind::Map(items) => {
                    items.iter().all(|(key, value)| safe(key) && safe(value))
                }
                _ => false,
            }
        }
        if !process.outputs.iter().all(|output| {
            output.optional.as_ref().is_none_or(safe)
                && output.parts.iter().all(|part| match part {
                    OutputPart::Value(value) | OutputPart::Eval(value) => safe(value),
                    OutputPart::Path { pattern, options }
                    | OutputPart::Files {
                        pattern, options, ..
                    } => {
                        safe(pattern)
                            && options.named.iter().all(|(_, value)| safe(value))
                            && options.positional.iter().all(safe)
                    }
                    OutputPart::Env(_) | OutputPart::Stdout => true,
                })
        }) {
            return Ok(None);
        }
        *task.frame.unset.borrow_mut() = process
            .section_variables
            .iter()
            .map(|(name, slot)| (*slot, name.clone()))
            .collect();
        let result = self.with_task(task.variables.clone(), task_object, |this| {
            for output in &process.outputs {
                for part in &output.parts {
                    if let OutputPart::Path { pattern, .. } | OutputPart::Files { pattern, .. } =
                        part
                        && matches!(
                            this.expression(&mut task.frame, pattern)?,
                            Value::Closure(_)
                        )
                    {
                        return Ok(None);
                    }
                }
            }
            this.render_outputs(task, process).map(Some)
        });
        self.in_script(&task.frame, result).map(|outputs| {
            outputs.map(|outputs| RenderedTask {
                run: true,
                script: String::new(),
                outputs,
            })
        })
    }

    /// A result of task code, whose error, like one of an output, happened in the script of
    /// the process unless a statement tells another.
    fn in_script<T>(&mut self, frame: &Frame, result: Result<T, Error>) -> Result<T, Error> {
        if result.is_err() && self.error_module.is_none() {
            self.error_module = Some(frame.module.clone());
        }
        result
    }

    /// The value of a `val` output. A variable that the section that ran did not set is a
    /// missing value, as Nextflow says.
    fn output_value(&mut self, frame: &mut Frame, value: &Expression) -> Result<Value, Error> {
        if let ExpressionKind::Variable(place) = &value.kind
            && let Some(name) = frame.unset_name(*place)
        {
            let message = format!("Missing value declared as output parameter: {name}");
            return Err(thrown(
                Exception::new("MissingValueException", message),
                value.span,
            ));
        }
        self.expression(frame, value)
    }

    /// The patterns of a `path` output: the value of a closure is taken when the task runs,
    /// and a list gives a pattern for each element.
    fn output_patterns(
        &mut self,
        frame: &mut Frame,
        pattern: &Expression,
    ) -> Result<Vec<String>, Error> {
        let value = match self.expression(frame, pattern)? {
            Value::Closure(closure) => {
                self.call_closure_value(&closure, Vec::new(), pattern.span)?
            }
            value => value,
        };
        Ok(match value {
            Value::List(list) => list.borrow().items.iter().map(Value::to_string).collect(),
            value => vec![value.to_string()],
        })
    }

    fn render_sections(
        &mut self,
        task: &mut TaskFrame,
        process: &Process,
        task_object: &Value,
        stub: bool,
    ) -> Result<RenderedTask, Error> {
        // As in Nextflow, only the section that runs sets its variables, so outputs that read
        // the variables of the other section fail.
        *task.frame.unset.borrow_mut() = process
            .section_variables
            .iter()
            .map(|(name, slot)| (*slot, name.clone()))
            .collect();
        let (statements, exec) = match (&process.stub, &process.body) {
            (Some(statements), _) if stub => (statements, false),
            (_, Some(body)) => (&body.statements, body.kind == BodyKind::Exec),
            // Legacy syntax: a process with only a stub runs it as its script.
            (Some(statements), None) => (statements, false),
            (None, None) => return Err(unsupported("processes without a script", process.span)),
        };
        // An `exec:` section is code that runs in the interpreter, as Nextflow runs it in its
        // own process: it makes its files in `task.workDir` and sets the variables of its
        // outputs, and has no script.
        let value = self.block(&mut task.frame, statements)?.value();
        // The prompt of an agent is the text of its last expression, without the shebang.
        if process.agent.is_some() {
            let prompt = strip_indent(&value.to_java_string());
            return Ok(RenderedTask {
                run: true,
                script: prompt.trim_matches(|c: char| c <= ' ').to_owned(),
                outputs: self.render_outputs(task, process)?,
            });
        }
        let script = if exec {
            String::new()
        } else {
            value.to_java_string()
        };
        let shell = match task_object {
            Value::Map(map) => map
                .borrow()
                .entries
                .get(&crate::value::Key(Value::string("shell")))
                .cloned(),
            _ => None,
        };
        let outputs = self.render_outputs(task, process)?;
        Ok(RenderedTask {
            run: true,
            script: normalise_script(&script, shell.as_ref()),
            outputs,
        })
    }

    fn render_outputs(
        &mut self,
        task: &mut TaskFrame,
        process: &Process,
    ) -> Result<Vec<RenderedOutput>, Error> {
        let mut outputs = Vec::new();
        for output in &process.outputs {
            let mut parts = Vec::new();
            for part in &output.parts {
                parts.push(match part {
                    OutputPart::Value(value) => {
                        RenderedPart::Value(self.output_value(&mut task.frame, value)?)
                    }
                    OutputPart::Path { pattern, options } => RenderedPart::Path {
                        patterns: self.output_patterns(&mut task.frame, pattern)?,
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
                    OutputPart::Files {
                        pattern,
                        options,
                        single,
                    } => RenderedPart::Files {
                        patterns: self.output_patterns(&mut task.frame, pattern)?,
                        options: (self.arguments(&mut task.frame, options)?.into_iter().next())
                            .unwrap_or(Value::map([])),
                        single: *single,
                    },
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
                typed: output.typed.clone(),
                hidden: output.hidden,
            });
        }
        Ok(outputs)
    }
}

/// The staged form of an input value: a path, or a `BlankSeparatedList` of paths, which is
/// also the form of one file when `single` is false. A name pattern names the files as
/// Nextflow's `expandWildcards` does.
fn stage(
    task: &mut TaskFrame,
    value: &Value,
    name: Option<&str>,
    single: bool,
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
    if staged.len() == 1 && single {
        return Ok(staged.remove(0));
    }
    let list = List::new(ListKind::BlankSeparated, staged);
    Ok(Value::List(Rc::new(crate::value::ListCell::new(list))))
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
        Value::Null => {
            let exception =
                Exception::new("ProcessUnrecoverableException", "Path value cannot be null");
            Err(thrown(exception, span))
        }
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
