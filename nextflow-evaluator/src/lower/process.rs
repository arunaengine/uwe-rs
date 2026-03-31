//! Lowering of processes in the legacy syntax that nf-core uses: `tuple val(meta), path(x)`
//! inputs and outputs, directives, `when:`, `script:` and `stub:`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_parser::Span;
use nextflow_parser::ast::{
    self, Argument, BodyKind, ExpressionKind as Syntax, NamedOutput, ProcessInput, StatementKind,
};

use super::Lowering;
use super::names::DIRECTIVES;
use crate::lowered::{Body, Directive, Input, Output, OutputPart, Process};

impl Lowering<'_> {
    /// Lowers a process in the order of Nextflow's `visitProcessV1`: inputs, `when:`, the
    /// script and stub, directives, then outputs, which may read the script's variables.
    pub(super) fn process(&mut self, process: &ast::Process) -> Process {
        self.scopes.push_frame(false);
        self.in_process = true;
        let inputs = process
            .inputs
            .iter()
            .filter_map(|input| self.input(input))
            .collect();
        if let Some(first) = process.stage.first().or(process.topics.first()) {
            self.error("Typed process sections are not supported yet", first.span);
        }
        let when = process.when.as_ref().map(|when| self.expression(when));
        self.implicit_declarations = true;
        let body = process.body.as_ref().map(|body| {
            self.escape_paths = matches!(body.kind, BodyKind::Script | BodyKind::Implicit);
            let statements = self.block(&body.block);
            self.escape_paths = false;
            Body {
                kind: body.kind,
                statements,
            }
        });
        let stub = process.stub.as_ref().map(|stub| {
            self.escape_paths = true;
            let statements = self.block(stub);
            self.escape_paths = false;
            statements
        });
        self.implicit_declarations = false;
        let directives = process
            .directives
            .iter()
            .filter_map(|directive| self.directive(directive))
            .collect();
        let outputs = process
            .outputs
            .iter()
            .filter_map(|output| self.output(output))
            .collect();
        self.in_process = false;
        let layout = self.scopes.pop_frame();
        Process {
            name: process.name.text.as_str().into(),
            slot_count: layout.slot_count,
            directives,
            inputs,
            outputs,
            when,
            body,
            stub,
            variables: layout.names,
            span: process.span,
        }
    }

    fn directive(&mut self, statement: &ast::Statement) -> Option<Directive> {
        let Some((name, arguments, span)) = call(statement) else {
            self.error("Invalid process directive", statement.span);
            return None;
        };
        if !DIRECTIVES.contains(&name) {
            self.error(format!("Unrecognized process directive `{name}`"), span);
            return None;
        }
        Some(Directive {
            name: name.into(),
            arguments: self.arguments(arguments),
            span: statement.span,
        })
    }

    fn input(&mut self, input: &ProcessInput) -> Option<Input> {
        match input {
            ProcessInput::Statement(statement) => match call(statement) {
                Some((name, arguments, span)) => self.input_call(name, arguments, span),
                None => {
                    self.error("Invalid process input", statement.span);
                    None
                }
            },
            ProcessInput::Named(typed) if typed.name.text == "stdin" => Some(Input::Stdin),
            ProcessInput::Named(typed) => {
                self.error(
                    "Typed process inputs are not supported yet",
                    typed.name.span,
                );
                None
            }
            ProcessInput::Record(fields) | ProcessInput::Tuple(fields) => {
                let span = fields
                    .first()
                    .map_or(Span::default(), |field| field.name.span);
                self.error("Typed process inputs are not supported yet", span);
                None
            }
        }
    }

    /// A legacy input like `val(x)`, `path(x, stageAs: '...')` or `tuple val(a), path(b)`.
    fn input_call(&mut self, name: &str, arguments: &[Argument], span: Span) -> Option<Input> {
        let positional: Vec<&ast::Expression> = arguments
            .iter()
            .filter(|argument| argument.name.is_none())
            .map(|argument| unparenthesized(&argument.value))
            .collect();
        let named: Vec<Argument> = arguments
            .iter()
            .filter(|a| a.name.is_some())
            .cloned()
            .collect();
        match (name, positional.as_slice()) {
            ("val", [value]) => match &value.kind {
                Syntax::Variable(variable) => Some(Input::Value {
                    slot: self.declare_name(variable, value.span),
                }),
                _ => self.invalid_input(span),
            },
            ("path" | "file", [value]) => {
                let options = self.arguments(&named);
                match &value.kind {
                    Syntax::Variable(variable) => Some(Input::Path {
                        slot: Some(self.declare_name(variable, value.span)),
                        pattern: None,
                        options,
                    }),
                    _ => Some(Input::Path {
                        slot: None,
                        pattern: Some(self.expression(value)),
                        options,
                    }),
                }
            }
            ("env", [value]) => self.env_name(value).map(Input::Env),
            ("stdin", []) => Some(Input::Stdin),
            ("tuple", parts) => {
                let mut inputs = Vec::new();
                for part in parts {
                    match &part.kind {
                        Syntax::MethodCall {
                            target: None,
                            name,
                            arguments,
                            ..
                        } => inputs.push(self.input_call(&name.text, arguments, part.span)?),
                        Syntax::Variable(variable) if variable == "stdin" => {
                            inputs.push(Input::Stdin);
                        }
                        _ => return self.invalid_input(part.span),
                    }
                }
                Some(Input::Tuple(inputs))
            }
            ("each", [value]) => match &value.kind {
                Syntax::Variable(variable) => Some(Input::Each(Box::new(Input::Value {
                    slot: self.declare_name(variable, value.span),
                }))),
                Syntax::MethodCall {
                    target: None,
                    name,
                    arguments,
                    ..
                } => {
                    let inner = self.input_call(&name.text, arguments, value.span)?;
                    Some(Input::Each(Box::new(inner)))
                }
                _ => self.invalid_input(span),
            },
            _ => {
                self.error(
                    format!("Unrecognized process input qualifier `{name}`"),
                    span,
                );
                None
            }
        }
    }

    /// The name of an `env` input or output, which must be a string: Nextflow resolves a bare
    /// name as a variable. The legacy parser turns a bare name into its text.
    fn env_name(&mut self, value: &ast::Expression) -> Option<Rc<str>> {
        let value = unparenthesized(value);
        match &value.kind {
            Syntax::Literal(ast::Literal::String(name)) => Some(name.as_str().into()),
            Syntax::Variable(name) => {
                self.legacy(format!("`{name}` is not defined"), value.span);
                Some(name.as_str().into())
            }
            _ => {
                self.error("Invalid environment variable name", value.span);
                None
            }
        }
    }

    fn invalid_input(&mut self, span: Span) -> Option<Input> {
        self.error("Invalid process input", span);
        None
    }

    /// Declares an input, which the legacy parser keeps in the task's binding. Legacy syntax
    /// may declare a name twice, as in `tuple val(meta), path(a)` and `tuple val(meta),
    /// path(b)`; the last input wins.
    fn declare_name(&mut self, name: &str, span: Span) -> usize {
        let name = ast::Name {
            text: name.to_owned(),
            span,
        };
        self.declare_as(&name, true)
    }

    fn output(&mut self, output: &NamedOutput) -> Option<Output> {
        let statement = match output {
            NamedOutput::Statement(statement) => statement,
            // A bare `stdout` reads like a typed output name.
            NamedOutput::Named {
                target,
                value: None,
            } if target.name.text == "stdout" && target.annotation.is_none() => {
                return Some(single(OutputPart::Stdout, target.name.span));
            }
            NamedOutput::Named { target, .. } => {
                self.error(
                    "Typed process outputs are not supported yet",
                    target.name.span,
                );
                return None;
            }
        };
        if let StatementKind::Expression(expression) = &statement.kind
            && let Syntax::Variable(name) = &expression.kind
            && name == "stdout"
        {
            return Some(single(OutputPart::Stdout, statement.span));
        }
        let Some((name, arguments, span)) = call(statement) else {
            self.error("Invalid process output", statement.span);
            return None;
        };
        // `emit:`, `topic:` and `optional:` belong to the whole output; `emit` and `topic`
        // take a plain name, which is not a variable.
        let mut output = Output {
            parts: Vec::new(),
            tuple: false,
            emit: None,
            topic: None,
            optional: None,
            span,
        };
        let mut part_options = Vec::new();
        for argument in arguments.iter().filter(|argument| argument.name.is_some()) {
            let key = argument.name.as_ref().map_or("", |name| name.text.as_str());
            match key {
                "emit" | "topic" => {
                    let Some(value) = name_or_text(&argument.value) else {
                        self.error(format!("Invalid `{key}` name"), argument.value.span);
                        continue;
                    };
                    let value = Some(Rc::from(value));
                    if key == "emit" {
                        output.emit = value
                    } else {
                        output.topic = value
                    }
                }
                "optional" => output.optional = Some(self.expression(&argument.value)),
                _ => part_options.push(argument.clone()),
            }
        }
        let positional: Vec<&ast::Expression> = arguments
            .iter()
            .filter(|argument| argument.name.is_none())
            .map(|argument| &argument.value)
            .collect();
        if name == "tuple" {
            output.tuple = true;
            for part in positional {
                match &part.kind {
                    Syntax::MethodCall {
                        target: None,
                        name,
                        arguments,
                        ..
                    } => output
                        .parts
                        .push(self.output_part(&name.text, arguments, part.span)?),
                    Syntax::Variable(variable) if variable == "stdout" => {
                        output.parts.push(OutputPart::Stdout);
                    }
                    _ => {
                        self.error("Invalid process output", part.span);
                        return None;
                    }
                }
            }
        } else {
            let mut all: Vec<Argument> = positional
                .into_iter()
                .map(|value| Argument {
                    name: None,
                    value: value.clone(),
                })
                .collect();
            all.extend(part_options);
            output.parts.push(self.output_part(name, &all, span)?);
        }
        Some(output)
    }

    fn output_part(
        &mut self,
        name: &str,
        arguments: &[Argument],
        span: Span,
    ) -> Option<OutputPart> {
        let positional: Vec<&ast::Expression> = arguments
            .iter()
            .filter(|argument| argument.name.is_none())
            .map(|argument| &argument.value)
            .collect();
        let named: Vec<Argument> = arguments
            .iter()
            .filter(|a| a.name.is_some())
            .cloned()
            .collect();
        Some(match (name, positional.as_slice()) {
            ("val", [value]) => OutputPart::Value(self.expression(value)),
            // Legacy syntax: a bare name that is no variable, like `path(bwa)`, is the file
            // name itself.
            ("path" | "file", [pattern]) => OutputPart::Path {
                pattern: match &unparenthesized(pattern).kind {
                    Syntax::Variable(file) if self.scopes.resolve(file).is_none() => {
                        let span = unparenthesized(pattern).span;
                        self.legacy(format!("`{file}` is not defined"), span);
                        crate::lowered::Expression {
                            kind: crate::lowered::ExpressionKind::Literal(
                                crate::value::Value::string(file.as_str()),
                            ),
                            span,
                        }
                    }
                    _ => self.expression(pattern),
                },
                options: self.arguments(&named),
            },
            ("env", [value]) => OutputPart::Env(self.env_name(value)?),
            ("eval", [command]) => OutputPart::Eval(self.expression(command)),
            ("stdout", []) => OutputPart::Stdout,
            _ => {
                self.error(
                    format!("Unrecognized process output qualifier `{name}`"),
                    span,
                );
                return None;
            }
        })
    }
}

/// An output with one part and no options.
fn single(part: OutputPart, span: Span) -> Output {
    Output {
        parts: vec![part],
        tuple: false,
        emit: None,
        topic: None,
        optional: None,
        span,
    }
}

/// The name and arguments of a statement that is a call without a receiver, like `cpus 2`.
fn call(statement: &ast::Statement) -> Option<(&str, &[Argument], Span)> {
    match &statement.kind {
        StatementKind::Expression(ast::Expression {
            kind:
                Syntax::MethodCall {
                    target: None,
                    name,
                    arguments,
                    ..
                },
            ..
        }) => Some((name.text.as_str(), arguments.as_slice(), name.span)),
        _ => None,
    }
}

/// The expression inside any parentheses, as in `path (indexes)`.
fn unparenthesized(expression: &ast::Expression) -> &ast::Expression {
    match &expression.kind {
        Syntax::Parenthesized(inner) => unparenthesized(inner),
        _ => expression,
    }
}

/// A name written as a variable or as a string literal, like `emit: reads` or `env 'FOO'`.
fn name_or_text(value: &ast::Expression) -> Option<&str> {
    match &value.kind {
        Syntax::Variable(name) => Some(name),
        Syntax::Literal(ast::Literal::String(text)) => Some(text),
        _ => None,
    }
}
