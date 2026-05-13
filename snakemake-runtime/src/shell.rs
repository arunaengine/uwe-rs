//! Formatting of shell commands and other job texts with Snakemake's rules: lists become
//! space-separated items, `:q` quotes for the shell, wildcards print as `name=value`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;
use std::rc::Rc;

use python_evaluator::{Args, Error, Interpreter, Scope, Value};

use crate::objects::NamedList;
use crate::workflow::State;

/// The command line that runs a shell command as Snakemake does: the executable (`bash`
/// unless `shell.executable()` names another) with `-c`, and the command between the prefix
/// (`set -euo pipefail; ` for bash, unless `shell.prefix()` sets one) and the suffix.
pub fn command_line(state: &State, command: &str) -> Vec<String> {
    let executable = state
        .shell_executable
        .clone()
        .unwrap_or_else(|| "bash".into());
    let bash = Path::new(&executable)
        .file_name()
        .is_some_and(|name| name == "bash");
    let prefix = state.shell_prefix.clone().unwrap_or_else(|| {
        if bash {
            "set -euo pipefail; ".into()
        } else {
            String::new()
        }
    });
    let full = format!("{prefix} {command} {}", state.shell_suffix)
        .trim()
        .to_owned();
    vec![executable, "-c".into(), full]
}

/// `shlex.quote`.
pub fn quote(text: &str) -> String {
    python_evaluator::modules::shell_quote(text)
}

fn format_element(
    interpreter: &mut Interpreter,
    value: &Value,
    spec: &str,
    quote_it: bool,
) -> Result<String, Error> {
    let text = python_evaluator::format::format_value(interpreter, value, spec)?;
    Ok(if quote_it && !text.is_empty() {
        quote(&text)
    } else {
        text
    })
}

/// `SequenceFormatter.format_field` with a `QuotedFormatter` for the items.
fn format_field(interpreter: &mut Interpreter, value: &Value, spec: &str) -> Result<String, Error> {
    let (spec, quote_it) = if let Some(spec) = spec.strip_suffix('u') {
        (spec, false)
    } else if let Some(spec) = spec.strip_suffix('q') {
        (spec, true)
    } else {
        (spec, false)
    };
    if let Some(list) = value.as_native::<NamedList>() {
        if list.kind == "Wildcards" {
            let mut pairs: Vec<(String, Value)> = list
                .keys()
                .into_iter()
                .map(|name| {
                    let value = list.named(&name).unwrap_or(Value::None);
                    (name, value)
                })
                .collect();
            pairs.sort_by(|(a, _), (b, _)| a.cmp(b));
            let mut parts = Vec::new();
            for (name, value) in pairs {
                parts.push(format!("{name}={}", interpreter.to_str(&value)?));
            }
            return Ok(parts.join(","));
        }
        let items = list.values.clone();
        let parts = items
            .iter()
            .map(|item| format_element(interpreter, item, spec, quote_it))
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(parts.join(" "));
    }
    match value.plain() {
        Value::List(_) | Value::Tuple(_) | Value::Set(_) | Value::FrozenSet(_) => {
            let items = interpreter.iterate(value)?;
            let parts = items
                .iter()
                .map(|item| format_element(interpreter, item, spec, quote_it))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(parts.join(" "))
        }
        _ => format_element(interpreter, value, spec, quote_it),
    }
}

/// Formats a text with named values, then the globals of the Snakefile, as
/// `snakemake.utils.format` does for shell commands.
pub fn format_command(
    interpreter: &mut Interpreter,
    template: &str,
    names: &Args,
    globals: Option<&Rc<Scope>>,
) -> Result<String, Error> {
    let mut resolve = |interpreter: &mut Interpreter, name: &str| -> Result<Value, Error> {
        if let Some(value) = names.keyword(name) {
            return Ok(value.clone());
        }
        if let Some(globals) = globals
            && let Some(value) = globals.get(name)
        {
            return Ok(value);
        }
        if let Some(value) = interpreter.builtins.get(name) {
            return Ok(value);
        }
        let wildcard = names
            .keyword("wildcards")
            .and_then(|wildcards| wildcards.as_native::<NamedList>().map(|list| list.keys()))
            .is_some_and(|keys| keys.iter().any(|key| key == name));
        Err(Error::new(
            "NameError",
            if wildcard {
                format!(
                    "The name '{name}' is unknown in this context. Did you mean 'wildcards.{name}'?"
                )
            } else {
                format!(
                    "The name '{name}' is unknown in this context. Please make sure that you defined that variable. Also note that braces not used for variable access have to be escaped by repeating them, i.e. {{{{print $1}}}}"
                )
            },
        ))
    };
    let mut field = |interpreter: &mut Interpreter, value: &Value, spec: &str| {
        format_field(interpreter, value, spec)
    };
    python_evaluator::format::vformat(interpreter, template, &mut resolve, &mut field)
}
