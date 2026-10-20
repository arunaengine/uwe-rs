//! Lowering of typed processes, which `nextflow.enable.types = true` turns on: typed inputs,
//! records and tuples, the `stage:` section, and outputs computed after the task.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_parser::ast::{self, ExpressionKind as Syntax, Item, ProcessInput};

use super::Lowering;
use super::process::call;
use crate::lowered::{Arguments, Expression, ExpressionKind, Input, Output, OutputPart};
use crate::value::Value;

/// Nextflow's error for typed declarations without the feature flag.
pub(super) const LEGACY_INPUT: &str = "Typed input declaration is not allowed in legacy process \
     -- set `nextflow.enable.types = true` to use typed processes in this script";

/// Whether a script sets `nextflow.enable.types = true`.
pub(super) fn enabled(script: &ast::Script) -> bool {
    script.items.iter().any(|item| match item {
        Item::FeatureFlag { name, value, .. } => {
            let name: Vec<&str> = name.iter().map(|part| part.text.as_str()).collect();
            name == ["enable", "types"]
                && matches!(value.kind, Syntax::Literal(ast::Literal::Boolean(true)))
        }
        _ => false,
    })
}

/// How many files a typed input takes: one for `Path`, any number for a collection of paths.
fn path_arity(annotation: Option<&ast::Type>) -> Option<&'static str> {
    let annotation = annotation?;
    let name = annotation.name.parts.last()?.text.as_str();
    let element = match annotation.arguments.first() {
        Some(ast::TypeArgument::Type(element)) => {
            element.name.parts.last().map(|p| p.text.as_str())
        }
        _ => None,
    };
    match (name, element) {
        ("Path", _) => Some("1"),
        ("Set" | "List" | "Bag" | "Collection" | "Iterable", Some("Path")) => Some("1..*"),
        _ => None,
    }
}

impl Lowering<'_> {
    /// A typed input: a path input for a `Path` or a collection of paths, else a value, or a
    /// `tuple(...)` or `record(...)` of them.
    pub(super) fn typed_input(&mut self, input: &ProcessInput) -> Option<Input> {
        match input {
            ProcessInput::Named(typed) => Some(self.typed_name(typed)),
            ProcessInput::Tuple(fields) => Some(Input::Tuple(
                fields.iter().map(|field| self.typed_name(field)).collect(),
            )),
            ProcessInput::Record(fields) => Some(Input::Record(
                (fields.iter())
                    .map(|field| (field.name.text.as_str().into(), self.typed_name(field)))
                    .collect(),
            )),
            ProcessInput::Statement(statement) => {
                self.error("Invalid process input", statement.span);
                None
            }
        }
    }

    fn typed_name(&mut self, typed: &ast::TypedName) -> Input {
        let slot = self.declare_name(&typed.name.text, typed.name.span);
        match path_arity(typed.annotation.as_ref()) {
            Some(arity) => {
                let value = Expression {
                    kind: ExpressionKind::Literal(Value::string(arity)),
                    span: typed.name.span,
                };
                Input::Path {
                    slot: Some(slot),
                    pattern: None,
                    options: Arguments {
                        named: vec![("arity".into(), value)],
                        positional: Vec::new(),
                    },
                    coerce: true,
                    listed: true,
                }
            }
            None => Input::Value { slot },
        }
    }

    /// The `stage:` section: `stageAs` names the files of a path input, and `env` and
    /// `stdin` give the task environment variables and its standard input.
    pub(super) fn stage(
        &mut self,
        statements: &[ast::Statement],
        inputs: &mut [Input],
    ) -> (Vec<(Rc<str>, Expression)>, Option<Expression>) {
        let mut environment = Vec::new();
        let mut stdin = None;
        for statement in statements {
            let Some((name, arguments, span)) = call(statement) else {
                self.error("Invalid stage directive", statement.span);
                continue;
            };
            let values: Vec<&ast::Expression> = arguments.iter().map(|a| &a.value).collect();
            match (name, values.as_slice()) {
                (
                    "stageAs",
                    [
                        ast::Expression {
                            kind: Syntax::Variable(variable),
                            span,
                        },
                        pattern,
                    ],
                ) => {
                    let slot = self.scopes.resolve(variable);
                    let pattern = self.expression(pattern);
                    let input = slot.and_then(|place| path_input(inputs, place));
                    match input {
                        Some(Input::Path {
                            pattern: target, ..
                        }) => *target = Some(pattern),
                        _ => self.error(format!("`{variable}` is not a path input"), *span),
                    }
                }
                ("env", [variable, value]) => match &variable.kind {
                    Syntax::Literal(ast::Literal::String(text)) => {
                        environment.push((text.as_str().into(), self.expression(value)));
                    }
                    _ => self.error(
                        "The name of an `env` stage directive must be a string",
                        span,
                    ),
                },
                ("stdin", [value]) => stdin = Some(self.expression(value)),
                _ => self.error(format!("Invalid stage directive `{name}`"), span),
            }
        }
        (environment, stdin)
    }
}

/// The path input that binds this variable, inside tuples and records too.
fn path_input(inputs: &mut [Input], place: crate::lowered::Place) -> Option<&mut Input> {
    for input in inputs {
        let found = match input {
            Input::Path {
                slot: Some(slot), ..
            } if crate::lowered::Place::Local(*slot) == place => {
                return Some(input);
            }
            Input::Tuple(parts) => path_input(parts, place),
            Input::Record(fields) => {
                let mut found = None;
                for (_, field) in fields.iter_mut() {
                    if let Some(input) = path_input(std::slice::from_mut(field), place) {
                        found = Some(input);
                        break;
                    }
                }
                found
            }
            _ => None,
        };
        if found.is_some() {
            return found;
        }
    }
    None
}

impl Lowering<'_> {
    /// A typed output: `name = value`, `name: Type = value`, a bare `name` that the script
    /// sets, or an expression without a name.
    pub(super) fn typed_output(&mut self, output: &ast::NamedOutput) -> Option<Output> {
        let (emit, value, span) = match output {
            ast::NamedOutput::Named { target, value } => {
                let name = Some(target.name.text.as_str().into());
                match value {
                    Some(value) => (name, value.clone(), target.name.span),
                    None => {
                        let variable = Syntax::Variable(target.name.text.clone());
                        let value = ast::Expression {
                            kind: variable,
                            span: target.name.span,
                        };
                        (name, value, target.name.span)
                    }
                }
            }
            ast::NamedOutput::Statement(ast::Statement {
                kind: ast::StatementKind::Expression(value),
                span,
            }) => (None, value.clone(), *span),
            ast::NamedOutput::Statement(statement) => {
                self.error("Invalid process output", statement.span);
                return None;
            }
        };
        Some(self.computed_output(emit, None, &value, span))
    }

    /// An entry of the `topic:` section, `value >> 'name'`, which only feeds the topic.
    pub(super) fn topic_entry(&mut self, statement: &ast::Statement) -> Option<Output> {
        if let ast::StatementKind::Expression(ast::Expression {
            kind:
                Syntax::Binary {
                    left,
                    operator: ast::BinaryOperator::RightShift,
                    right,
                },
            ..
        }) = &statement.kind
            && let Syntax::Literal(ast::Literal::String(topic)) = &right.kind
        {
            let mut output =
                self.computed_output(None, Some(topic.as_str().into()), left, statement.span);
            output.hidden = true;
            return Some(output);
        }
        self.error("Invalid topic emission", statement.span);
        None
    }

    /// An output whose value comes from code that runs after the task.
    fn computed_output(
        &mut self,
        emit: Option<Rc<str>>,
        topic: Option<Rc<str>>,
        value: &ast::Expression,
        span: nextflow_parser::Span,
    ) -> Output {
        self.task_outputs = Some(Vec::new());
        let value = self.expression(value);
        let parts = self.task_outputs.take().unwrap_or_default();
        Output {
            parts,
            tuple: true,
            emit,
            topic,
            optional: None,
            typed: Some(Rc::new(value)),
            hidden: false,
            span,
        }
    }

    /// In a typed output, `file()`, `files()`, `stdout()`, `env()` and `eval()` read what the
    /// task made; any other call stays as it is.
    pub(super) fn task_output(
        &mut self,
        name: &str,
        span: nextflow_parser::Span,
        arguments: Arguments,
    ) -> Result<ExpressionKind, Arguments> {
        if self.task_outputs.is_none() {
            return Err(arguments);
        }
        let Arguments {
            named,
            mut positional,
        } = arguments;
        let part = match (name, positional.len()) {
            ("file" | "files", 1) => OutputPart::Files {
                pattern: positional.remove(0),
                options: Arguments {
                    named,
                    positional: Vec::new(),
                },
                single: name == "file",
            },
            ("stdout", 0) => OutputPart::Stdout,
            ("eval", 1) => OutputPart::Eval(positional.remove(0)),
            ("env", 1) => match &positional[0].kind {
                ExpressionKind::Literal(Value::String(text)) => OutputPart::Env(text.clone()),
                _ => {
                    self.error("The name of an `env` output must be a string", span);
                    OutputPart::Stdout
                }
            },
            _ => return Err(Arguments { named, positional }),
        };
        let parts = self.task_outputs.as_mut().expect("lowering a typed output");
        parts.push(part);
        Ok(ExpressionKind::TaskOutput(parts.len() - 1))
    }
}
