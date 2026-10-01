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
            .filter(|input| {
                !matches!(input, ProcessInput::Statement(statement) if statement.is_variable_statement())
            })
            .filter_map(|input| self.input(input))
            .collect();
        if let Some(first) = process.stage.first().or(process.topics.first()) {
            self.error("Typed process sections are not supported yet", first.span);
        }
        let when = process.when.as_ref().map(|when| match &when.kind {
            // Legacy syntax: a directive after `when:` belongs to the guard, where Nextflow
            // 26.04.6 gives null for it, so the process runs no task.
            Syntax::MethodCall {
                target: None, name, ..
            } if DIRECTIVES.contains(&name.text.as_str()) => {
                self.legacy("Invalid process directive", when.span);
                crate::lowered::Expression {
                    kind: crate::lowered::ExpressionKind::Literal(crate::value::Value::Null),
                    span: when.span,
                }
            }
            _ => self.expression(when),
        });
        self.implicit_declarations = true;
        // Legacy syntax: a variable that is declared or assigned among the directives, the
        // inputs or the outputs, like `def threshold = params.t`, which later sections read.
        // It is set for each task before the directives.
        let variables: Vec<ast::Statement> = process
            .directives
            .iter()
            .chain(process.inputs.iter().filter_map(|input| match input {
                ProcessInput::Statement(statement) => Some(statement),
                _ => None,
            }))
            .chain(process.outputs.iter().filter_map(|output| match output {
                NamedOutput::Statement(statement) => Some(statement),
                NamedOutput::Named { .. } => None,
            }))
            .filter(|statement| statement.is_variable_statement())
            .cloned()
            .collect();
        let mut setup = self.statements(&variables);
        let first_section_name = self.scopes.frame_names();
        let body = process.body.as_ref().map(|body| {
            self.escape_paths = matches!(body.kind, BodyKind::Script | BodyKind::Implicit);
            let statements = match body.kind {
                BodyKind::Shell => {
                    let mut block = body.block.clone();
                    if let Some(last) = block.statements.last_mut() {
                        shell_statement(last);
                    }
                    self.block(&block)
                }
                _ => self.block(&body.block),
            };
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
        let section_names = first_section_name..self.scopes.frame_names();
        self.implicit_declarations = false;
        let mut directives = Vec::new();
        for directive in &process.directives {
            if directive.is_variable_statement() {
                continue;
            }
            // Legacy syntax: a string among the directives, like a description, does nothing.
            if let StatementKind::Expression(ast::Expression {
                kind: Syntax::Literal(ast::Literal::String(_)) | Syntax::GString(_),
                ..
            }) = &directive.kind
            {
                continue;
            }
            if let StatementKind::If { .. } = directive.kind {
                self.legacy("Invalid process directive", directive.span);
                let mut guarded = Vec::new();
                if guarded_directives(directive, None, &mut guarded) {
                    for (statement, guard) in guarded {
                        if let Some(mut lowered) = self.directive(&statement) {
                            lowered.guard = guard.map(|guard| self.expression(&guard));
                            directives.push(lowered);
                        }
                    }
                } else {
                    setup.extend(self.statements(std::slice::from_ref(directive)));
                }
                continue;
            }
            directives.extend(self.directive(directive));
        }
        // Legacy syntax: a directive among the outputs, like `publishDir`, is a directive, as
        // the legacy parser runs it on the process config.
        let mut outputs = Vec::new();
        for output in &process.outputs {
            match output {
                NamedOutput::Statement(statement) if statement.is_variable_statement() => {}
                NamedOutput::Statement(statement)
                    if call(statement).is_some_and(|(name, _, _)| {
                        DIRECTIVES.contains(&name) && !OUTPUT_QUALIFIERS.contains(&name)
                    }) =>
                {
                    self.legacy("Invalid process output", statement.span);
                    directives.extend(self.directive(statement));
                }
                _ => outputs.extend(self.output(output)),
            }
        }
        self.in_process = false;
        let layout = self.scopes.pop_frame();
        let section_variables = layout.names[section_names].to_vec();
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
            section_variables,
            setup,
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
            guard: None,
            span: statement.span,
        })
    }

    fn input(&mut self, input: &ProcessInput) -> Option<Input> {
        match input {
            ProcessInput::Statement(statement) => match call(statement) {
                Some((name, arguments, span)) => self.input_call(name, arguments, span),
                // Legacy syntax: the legacy parser ignores a string among the inputs.
                None if matches!(
                    &statement.kind,
                    ast::StatementKind::Expression(ast::Expression {
                        kind: Syntax::Literal(ast::Literal::String(_)) | Syntax::GString(_),
                        ..
                    })
                ) =>
                {
                    self.legacy("Invalid process input", statement.span);
                    None
                }
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
                        coerce: name == "path",
                    }),
                    _ => Some(Input::Path {
                        slot: None,
                        pattern: Some(self.expression(value)),
                        options,
                        coerce: name == "path",
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
            // Legacy syntax: a bare name is the name of the variable.
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
        // Legacy syntax: `path "*.pdf" optional true` calls a method on the output, which
        // strict syntax gives as the option `optional: true`. Nextflow 23.04 reads it so, and
        // 26.04.6 fails on it; iunctus keeps the meaning of the pipelines' releases.
        let folded;
        let statement = match &statement.kind {
            StatementKind::Expression(
                expression @ ast::Expression {
                    kind:
                        Syntax::MethodCall {
                            target: Some(_), ..
                        },
                    ..
                },
            ) => {
                self.legacy("Invalid process output", statement.span);
                folded = ast::Statement {
                    kind: StatementKind::Expression(option_calls(expression.clone())),
                    span: statement.span,
                };
                &folded
            }
            _ => statement,
        };
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
pub(super) fn call(statement: &ast::Statement) -> Option<(&str, &[Argument], Span)> {
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

/// The qualifiers of process outputs, which are not directives there.
const OUTPUT_QUALIFIERS: &[&str] = &[
    "val", "path", "file", "env", "stdout", "tuple", "set", "eval",
];

/// Legacy syntax: the directives inside the `if` statements of a process, each with the
/// condition under which it applies. False when a branch holds anything but directives.
fn guarded_directives(
    statement: &ast::Statement,
    guard: Option<ast::Expression>,
    found: &mut Vec<(ast::Statement, Option<ast::Expression>)>,
) -> bool {
    let both = |guard: &Option<ast::Expression>, condition: ast::Expression| {
        let span = condition.span;
        Some(match guard {
            Some(guard) => ast::Expression {
                kind: Syntax::Binary {
                    operator: ast::BinaryOperator::And,
                    left: Box::new(guard.clone()),
                    right: Box::new(condition),
                },
                span,
            },
            None => condition,
        })
    };
    match &statement.kind {
        StatementKind::If {
            condition,
            then,
            otherwise,
        } => {
            let negated = ast::Expression {
                kind: Syntax::Unary {
                    operator: ast::UnaryOperator::Not,
                    operand: Box::new(condition.clone()),
                },
                span: condition.span,
            };
            let branches = [
                (Some(then), both(&guard, condition.clone())),
                (otherwise.as_ref(), both(&guard, negated)),
            ];
            branches.into_iter().all(|(block, guard)| {
                block.is_none_or(|block| {
                    (block.statements.iter())
                        .all(|statement| guarded_directives(statement, guard.clone(), found))
                })
            })
        }
        _ => match call(statement) {
            Some((name, _, _)) if DIRECTIVES.contains(&name) => {
                found.push((statement.clone(), guard));
                true
            }
            _ => false,
        },
    }
}

/// Legacy syntax: the calls on an output, like `path("*.pdf").optional(true)` of
/// `path "*.pdf" optional true`, as named options of the output: `path("*.pdf", optional:
/// true)`. A call without an argument, like `optional`, gives `true`.
fn option_calls(expression: ast::Expression) -> ast::Expression {
    match expression.kind {
        Syntax::MethodCall {
            target: Some(target),
            name,
            arguments,
            ..
        } => {
            let mut inner = option_calls(*target);
            if let Syntax::MethodCall {
                arguments: options, ..
            } = &mut inner.kind
            {
                let value = arguments.into_iter().next().map_or(
                    ast::Expression {
                        kind: Syntax::Literal(ast::Literal::Boolean(true)),
                        span: name.span,
                    },
                    |argument| argument.value,
                );
                options.push(Argument {
                    name: Some(name),
                    value,
                });
            }
            inner
        }
        kind => ast::Expression {
            kind,
            span: expression.span,
        },
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

/// Lowers shell-section strings with `!{expression}` interpolation, keeping `$` for the shell.
/// This matches Nextflow's TaskTemplateEngine with `!` as its placeholder.
fn shell_statement(statement: &mut ast::Statement) {
    match &mut statement.kind {
        StatementKind::Expression(expression) | StatementKind::Return(Some(expression)) => {
            shell_template(expression);
        }
        StatementKind::If {
            then, otherwise, ..
        } => {
            for block in std::iter::once(then).chain(otherwise) {
                if let Some(last) = block.statements.last_mut() {
                    shell_statement(last);
                }
            }
        }
        _ => {}
    }
}

fn shell_template(expression: &mut ast::Expression) {
    match &mut expression.kind {
        Syntax::Literal(ast::Literal::String(text)) if text.contains("!{") => {
            expression.kind = Syntax::GString(shell_parts(text));
        }
        Syntax::GString(parts) => {
            *parts = std::mem::take(parts)
                .into_iter()
                .flat_map(|part| match part {
                    ast::GStringPart::Text(text) => shell_parts(&text),
                    value => vec![value],
                })
                .collect();
        }
        Syntax::Parenthesized(inner) => shell_template(inner),
        Syntax::Ternary {
            then, otherwise, ..
        } => {
            shell_template(then);
            shell_template(otherwise);
        }
        _ => {}
    }
}

/// The parts of a shell text: `!{expression}` up to the next `}` is a value, as in
/// `EscapeTemplate.processGString`, which drops the line breaks inside.
fn shell_parts(text: &str) -> Vec<ast::GStringPart> {
    let mut parts = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("!{") {
        let Some(length) = rest[start..].find('}') else {
            break;
        };
        let source: String = rest[start + 2..start + length]
            .chars()
            .filter(|c| !matches!(c, '\n' | '\r'))
            .collect();
        let parsed = nextflow_parser::parse_script(&source);
        let value = match parsed.tree.items.as_slice() {
            [
                ast::Item::Statement(ast::Statement {
                    kind: StatementKind::Expression(value),
                    ..
                }),
            ] if parsed.errors.is_empty() => value.clone(),
            _ => break,
        };
        if start > 0 {
            parts.push(ast::GStringPart::Text(rest[..start].to_owned()));
        }
        parts.push(ast::GStringPart::Value(value));
        rest = &rest[start + length + 1..];
    }
    if !rest.is_empty() {
        parts.push(ast::GStringPart::Text(rest.to_owned()));
    }
    parts
}
