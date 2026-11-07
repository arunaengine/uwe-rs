//! Lowers a CWL command line tool into a wir task, when the task builds the same command.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cmp::Ordering;

use cwl_parser::model::{
    Argument as CwlArgument, CommandLineBinding, CommandLineTool, Dynamic, Expression, Part,
    Process, Type as Cwl, short_name,
};
use wir::{Argument, Attributes, Capture, CaptureRule, FileType, Port, Task, Type};

use crate::Scope;
use crate::requirements;
use crate::types::convert;

/// The image of tools without `DockerRequirement`, as uwe runs them.
pub const DEFAULT_IMAGE: &str = "debian:stable-slim";

/// A tool as a task with its ports and the attributes from its requirements.
pub struct Lowered {
    pub task: Task,
    pub inputs: Vec<Port>,
    pub outputs: Vec<Port>,
    pub attributes: Attributes,
}

/// The task of a tool, or the first thing it uses that a task cannot say the same way.
pub fn lower(process: &Process, tool: &CommandLineTool, scope: &Scope) -> Result<Lowered, String> {
    let mut task = Task {
        image: Some(DEFAULT_IMAGE.to_owned()),
        command: Vec::new(),
        captures: Vec::new(),
        success_codes: Vec::new(),
        network: false,
        stdout: literal(tool.stdout.as_ref(), "stdout")?,
        stderr: literal(tool.stderr.as_ref(), "stderr")?,
    };
    let attributes = requirements::apply(scope, &mut task)?;
    if tool.stdin.is_some() {
        return Err("stdin".into());
    }
    if !tool.temporary_fail_codes.is_empty() || !tool.permanent_fail_codes.is_empty() {
        return Err("temporary or permanent fail codes".into());
    }
    task.success_codes = (tool.success_codes.iter())
        .map(|code| i32::try_from(*code).map_err(|_| format!("the success code {code}")))
        .collect::<Result<_, _>>()?;
    let mut words: Vec<(Vec<Key>, Vec<Argument>)> = Vec::new();
    for (index, word) in tool.base_command.iter().enumerate() {
        let key = vec![Key::Number(-1_000_000), Key::Number(index as i64)];
        words.push((key, vec![Argument::Text(word.clone())]));
    }
    for (index, argument) in tool.arguments.iter().enumerate() {
        let (position, text) = match argument {
            CwlArgument::Text(text) => (0, text.clone()),
            CwlArgument::Binding(binding) => match &binding.value_from {
                Some(text) => (position(binding)?, text.clone()),
                None => continue,
            },
        };
        let text = literal(Some(&text), "an argument")?.unwrap_or_default();
        let key = vec![Key::Number(position), Key::Number(index as i64)];
        let words_of = match argument {
            CwlArgument::Binding(binding) => prefixed(binding, Argument::Text(text))?,
            CwlArgument::Text(_) => vec![Argument::Text(text)],
        };
        words.push((key, words_of));
    }
    let mut inputs = Vec::new();
    for input in &process.inputs {
        let name = short_name(&input.id).to_owned();
        if input.default.is_some() {
            return Err(format!("the default of input {name}"));
        }
        if !input.secondary_files.is_empty() || !input.format.is_empty() {
            return Err(format!("the secondary files or format of input {name}"));
        }
        if let Some(binding) = &input.binding {
            let key = vec![Key::Number(position(binding)?), Key::Name(name.clone())];
            words.push((key, bound(binding, &input.value_type, &name)?));
        }
        let r#type = convert(&input.value_type).map_err(|why| format!("input {name}: {why}"))?;
        inputs.push(Port {
            name,
            r#type,
            flow: wir::Flow::Value,
        });
    }
    words.sort_by(|a, b| compare(&a.0, &b.0));
    task.command = words.into_iter().flat_map(|(_, words)| words).collect();
    let mut outputs = Vec::new();
    for output in &process.outputs {
        let name = short_name(&output.id).to_owned();
        let (rule, r#type) =
            capture(output, &task).map_err(|why| format!("output {name}: {why}"))?;
        task.captures.push(Capture {
            output: name.clone(),
            from: rule,
        });
        outputs.push(Port {
            name,
            r#type,
            flow: wir::Flow::Value,
        });
    }
    Ok(Lowered {
        task,
        inputs,
        outputs,
        attributes,
    })
}

/// The words of an input binding: the value of the input, after its prefix.
fn bound(
    binding: &CommandLineBinding,
    value_type: &Cwl,
    name: &str,
) -> Result<Vec<Argument>, String> {
    if binding.value_from.is_some() || binding.item_separator.is_some() {
        return Err(format!("the valueFrom or itemSeparator of input {name}"));
    }
    let members: Vec<&Cwl> = match value_type {
        Cwl::Union(members) => members
            .iter()
            .filter(|member| **member != Cwl::Null)
            .collect(),
        other => vec![other],
    };
    let optional = matches!(value_type, Cwl::Union(members) if members.contains(&Cwl::Null));
    let input = Argument::Input {
        input: name.to_owned(),
    };
    match members.as_slice() {
        [Cwl::Array(array)]
            if binding.prefix.is_none() && array.binding.is_none() && plain(&array.items) =>
        {
            Ok(vec![input])
        }
        [single] if plain(single) && !(optional && binding.prefix.is_some()) => {
            prefixed(binding, input)
        }
        _ => Err(format!("the command line binding of input {name}")),
    }
}

/// Types whose value is one word on the command line in CWL and in wir alike.
fn plain(r#type: &Cwl) -> bool {
    match r#type {
        Cwl::Int | Cwl::Long | Cwl::Float | Cwl::Double | Cwl::String | Cwl::File => true,
        Cwl::Directory => true,
        Cwl::Enum(schema) => schema.binding.is_none(),
        _ => false,
    }
}

/// A word after the prefix of a binding, as a separate word.
fn prefixed(binding: &CommandLineBinding, word: Argument) -> Result<Vec<Argument>, String> {
    match (&binding.prefix, binding.separate) {
        (None, _) => Ok(vec![word]),
        (Some(prefix), true) => Ok(vec![Argument::Text(prefix.clone()), word]),
        (Some(prefix), false) => match word {
            Argument::Text(text) => Ok(vec![Argument::Text(format!("{prefix}{text}"))]),
            Argument::Input { .. } => Err("a prefix joined to an input value".into()),
        },
    }
}

fn position(binding: &CommandLineBinding) -> Result<i64, String> {
    match &binding.position {
        None => Ok(0),
        Some(Dynamic::Value(position)) => Ok(*position),
        Some(Dynamic::Expression(_)) => Err("a position set by an expression".into()),
    }
}

/// The text of an expression without code; `what` names it in the reason.
fn literal(expression: Option<&Expression>, what: &str) -> Result<Option<String>, String> {
    let Some(expression) = expression else {
        return Ok(None);
    };
    let mut text = String::new();
    for part in &expression.parts {
        match part {
            Part::Text(piece) => text.push_str(piece),
            _ => return Err(format!("an expression in {what}")),
        }
    }
    Ok(Some(text))
}

/// How a task reads an output, and the output's type.
fn capture(
    output: &cwl_parser::model::OutputParameter,
    task: &Task,
) -> Result<(CaptureRule, Type), String> {
    let file = |format: Option<String>| {
        Type::File(FileType {
            format,
            secondary_files: Vec::new(),
        })
    };
    if !output.secondary_files.is_empty() {
        return Err("secondary files".into());
    }
    let format = literal(output.format.as_ref(), "the format")?;
    match &output.value_type {
        Cwl::Stdout if output.binding.is_none() => return Ok((CaptureRule::Stdout, file(format))),
        Cwl::Stderr if output.binding.is_none() => return Ok((CaptureRule::Stderr, file(format))),
        _ => {}
    }
    let Some(binding) = &output.binding else {
        return Err("no glob".into());
    };
    if binding.output_eval.is_some() || binding.load_contents == Some(true) {
        return Err("outputEval or loadContents".into());
    }
    let [glob] = binding.glob.as_slice() else {
        return Err("not one glob".into());
    };
    let pattern = literal(Some(glob), "a glob")?.unwrap_or_default();
    if pattern.is_empty() || pattern.starts_with('/') || pattern == "." || pattern.contains("..") {
        return Err(format!("the glob {pattern:?}"));
    }
    let r#type = convert(&output.value_type)?;
    let r#type = with_format(r#type, format);
    let fits = match &r#type {
        Type::Optional(inner) => matches!(**inner, Type::File(_) | Type::Directory),
        Type::Array(array) => matches!(*array.items, Type::File(_) | Type::Directory),
        other => matches!(other, Type::File(_) | Type::Directory),
    };
    match fits {
        // A stream file is also a file in the working folder, which a glob may match.
        true if task.stdout.as_deref() == Some(&pattern) => Ok((CaptureRule::Stdout, r#type)),
        true => Ok((CaptureRule::Glob(pattern), r#type)),
        false => Err("a type that a glob does not fill".into()),
    }
}

/// The type with the format on its File, also inside an optional or an array.
fn with_format(r#type: Type, format: Option<String>) -> Type {
    match r#type {
        Type::File(_) => Type::File(FileType {
            format,
            secondary_files: Vec::new(),
        }),
        Type::Optional(inner) => Type::Optional(Box::new(with_format(*inner, format))),
        Type::Array(mut array) => {
            array.items = Box::new(with_format(*array.items, format));
            Type::Array(array)
        }
        other => other,
    }
}

/// Where a word goes on the command line, as cwltool sorts bindings.
#[derive(Clone, Debug, PartialEq)]
enum Key {
    Number(i64),
    Name(String),
}

impl Key {
    fn text(&self) -> String {
        match self {
            Key::Number(number) => number.to_string(),
            Key::Name(name) => name.clone(),
        }
    }
}

/// Key lists compare element by element; a shorter list comes first, and a name compares as
/// text with anything, as in uwe's CWL driver.
fn compare(a: &[Key], b: &[Key]) -> Ordering {
    for index in 0..a.len().max(b.len()) {
        match (a.get(index), b.get(index)) {
            (Some(x), Some(y)) if x == y => continue,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(Key::Number(x)), Some(Key::Number(y))) => return x.cmp(y),
            (Some(x), Some(y)) => {
                return match x.text() > y.text() {
                    true => Ordering::Greater,
                    false => Ordering::Less,
                };
            }
        }
    }
    Ordering::Equal
}
