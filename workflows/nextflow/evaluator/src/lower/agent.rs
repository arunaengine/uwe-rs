//! Lowering of `agent` definitions into processes whose `script:` is the prompt to a language
//! model: directives, typed inputs, the one output and the types it may answer.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::rc::Rc;

use nextflow_parser::ast::{self, BodyKind, NamedOutput, ProcessInput, TypeArgument};

use super::Lowering;
use super::names::DIRECTIVES;
use super::process::call;
use crate::lowered::{
    Agent, AnswerField, AnswerType, Body, Directive, Output, OutputPart, Process, Structured,
};

/// The record types of a script by name, with their fields.
pub(super) type Records<'s> = HashMap<&'s str, &'s [ast::TypedName]>;

/// The directives of an agent body besides the task directives of a process.
pub const AGENT_DIRECTIVES: &[&str] = &[
    "model",
    "instruction",
    "goal",
    "maxIterations",
    "tools",
    "skills",
];

const SUPPORTED: &str =
    "supported types are Boolean, Float, Integer, List<E>, Path, String, or a record type";

/// How deep record types may nest, which also stops a record that contains itself.
const DEPTH: usize = 16;

impl Lowering<'_> {
    /// Lowers an agent like a typed process: inputs, the prompt as its script, directives,
    /// then the output, which may read the variables of the prompt section.
    pub(super) fn agent(&mut self, agent: &ast::Agent, records: &Records) -> Process {
        self.scopes.push_frame(false);
        self.in_process = true;
        let inputs = (agent.inputs.iter())
            .filter_map(|input| self.typed_input(input))
            .collect();
        let names = (agent.inputs.iter())
            .map(|input| match input {
                ProcessInput::Named(typed) => vec![typed.name.text.as_str().into()],
                ProcessInput::Record(fields) | ProcessInput::Tuple(fields) => (fields.iter())
                    .map(|field| field.name.text.as_str().into())
                    .collect(),
                ProcessInput::Statement(_) => Vec::new(),
            })
            .collect();
        self.implicit_declarations = true;
        let first = self.scopes.frame_names();
        let prompt = self.block(&agent.prompt);
        let section_names = first..self.scopes.frame_names();
        self.implicit_declarations = false;
        let directives = (agent.directives.iter())
            .filter_map(|statement| self.agent_directive(statement))
            .collect();
        let (outputs, structured) = self.agent_output(agent, records);
        self.in_process = false;
        let layout = self.scopes.pop_frame();
        let section_variables = layout.names[section_names].to_vec();
        Process {
            name: agent.name.text.as_str().into(),
            slot_count: layout.slot_count,
            directives,
            inputs,
            outputs,
            when: None,
            body: Some(Body {
                kind: BodyKind::Script,
                statements: prompt,
            }),
            stub: None,
            variables: layout.names,
            section_variables,
            setup: Vec::new(),
            stage_env: Vec::new(),
            stage_stdin: None,
            non_null: super::typed::non_null(&agent.inputs),
            agent: Some(Agent {
                inputs: names,
                structured,
            }),
            span: agent.span,
        }
    }

    /// An agent directive, or a task directive of a process, which the agent docs let the
    /// definition set too.
    fn agent_directive(&mut self, statement: &ast::Statement) -> Option<Directive> {
        if matches!(statement.kind, ast::StatementKind::Empty) {
            return None;
        }
        let Some((name, arguments, span)) = call(statement) else {
            self.error("Invalid agent directive", statement.span);
            return None;
        };
        if !AGENT_DIRECTIVES.contains(&name) && !DIRECTIVES.contains(&name) {
            self.error(format!("Unrecognized agent directive `{name}`"), span);
            return None;
        }
        Some(Directive {
            name: name.into(),
            arguments: self.arguments(arguments),
            guard: None,
            span: statement.span,
        })
    }

    /// The output of an agent: a typed name, which the model answers as JSON, or `stdout()`
    /// and records of it, which take its text.
    fn agent_output(
        &mut self,
        agent: &ast::Agent,
        records: &Records,
    ) -> (Vec<Output>, Option<Structured>) {
        let Some(output) = agent.outputs.first() else {
            return (Vec::new(), None);
        };
        if let NamedOutput::Named {
            target,
            value: None,
        } = output
            && let Some(annotation) = &target.annotation
        {
            let name: Rc<str> = target.name.text.as_str().into();
            let kind = self.answer_type(annotation, records, 0);
            if kind.is_none() {
                let text = type_text(annotation);
                let message =
                    format!("Agent output `{name}` has unsupported type {text} -- {SUPPORTED}");
                self.error(message, target.name.span);
            }
            let output = Output {
                parts: vec![OutputPart::Stdout],
                tuple: false,
                emit: Some(name.clone()),
                topic: None,
                optional: None,
                typed: None,
                hidden: false,
                span: target.name.span,
            };
            return (vec![output], kind.map(|kind| Structured { name, kind }));
        }
        let output = self.typed_output(output);
        if let Some(output) = &output
            && (output.parts.iter()).any(|part| matches!(part, OutputPart::Files { .. }))
        {
            let message = format!(
                "Agent `{}`: file outputs need the file tools of agents (`fs:write`), which \
                 uwe does not run yet",
                agent.name.text
            );
            self.error(message, output.span);
        }
        (output.into_iter().collect(), None)
    }

    /// The answer type of a type annotation; `None` when an agent cannot answer it.
    fn answer_type(
        &mut self,
        annotation: &ast::Type,
        records: &Records,
        depth: usize,
    ) -> Option<AnswerType> {
        let name = annotation.name.parts.last()?.text.as_str();
        if annotation.dimensions > 0 {
            return None;
        }
        Some(match name {
            "String" => AnswerType::Text,
            "Boolean" => AnswerType::Boolean,
            "Integer" | "Long" | "Short" | "Byte" | "BigInteger" => AnswerType::Integer,
            "Float" | "Double" | "BigDecimal" | "Number" => AnswerType::Float,
            "Path" => AnswerType::Path,
            "List" | "Set" | "Collection" | "Bag" | "Iterable" => {
                let Some(TypeArgument::Type(element)) = annotation.arguments.first() else {
                    return None;
                };
                AnswerType::List(Box::new(self.answer_type(element, records, depth)?))
            }
            _ => {
                let fields = records.get(name).filter(|_| depth < DEPTH)?;
                let fields = (fields.iter())
                    .filter_map(|field| self.answer_field(field, records, depth + 1))
                    .collect();
                AnswerType::Record(fields)
            }
        })
    }

    fn answer_field(
        &mut self,
        field: &ast::TypedName,
        records: &Records,
        depth: usize,
    ) -> Option<AnswerField> {
        let kind = (field.annotation.as_ref())
            .and_then(|annotation| self.answer_type(annotation, records, depth));
        let Some(kind) = kind else {
            let text = field.annotation.as_ref().map_or("Object".into(), type_text);
            let name = &field.name.text;
            let message =
                format!("Unsupported agent output field `{name}` of type {text} -- {SUPPORTED}");
            self.error(message, field.name.span);
            return None;
        };
        Some(AnswerField {
            name: field.name.text.as_str().into(),
            kind,
            optional: field.annotation.as_ref().is_some_and(|a| a.nullable),
        })
    }
}

/// A type as the script writes it, like `List<String>` or `Path?`.
fn type_text(annotation: &ast::Type) -> String {
    let parts: Vec<&str> = (annotation.name.parts.iter())
        .map(|part| part.text.as_str())
        .collect();
    let mut text = parts.join(".");
    if !annotation.arguments.is_empty() {
        let arguments: Vec<String> = (annotation.arguments.iter())
            .map(|argument| match argument {
                TypeArgument::Type(inner) => type_text(inner),
                TypeArgument::Wildcard => "?".into(),
            })
            .collect();
        text.push_str(&format!("<{}>", arguments.join(", ")));
    }
    text.push_str(&"[]".repeat(annotation.dimensions as usize));
    if annotation.nullable {
        text.push('?');
    }
    text
}
