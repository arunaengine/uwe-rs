//! The outputs of a WDL task: which ones the task op captures with a glob, which the runner
//! copies from paths an expr op computes, and which are values of expr ops.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeSet;

use wdl_ast::AstToken;
use wdl_ast::v1::{Expr, LiteralExpr, TaskDefinition};
use wdl_runtime::Value;
use wdl_runtime::document::{Unit, declared};
use wdl_runtime::expression::names;
use wir::Type;

use crate::types::{convert, has_files};
use crate::{constant, reads, text};

/// How the task op reads an output, or how an expr op computes it.
#[derive(Clone, Debug)]
pub(crate) enum Read {
    /// A file that the task op captures with a glob.
    Glob(String),
    /// The files that a `glob` call matches, which the runner copies before it places others.
    Matched(String),
    /// Files whose paths an expr op computes before the run, which the runner copies.
    Placed,
    /// A value without files that an expr op computes from the inputs and other outputs.
    Value,
}

pub(crate) struct Output {
    pub name: String,
    pub r#type: Type,
    pub read: Read,
    /// The declaration as WDL text, and the outputs it refers to.
    pub decl: String,
    pub uses: BTreeSet<String>,
    pub code: String,
}

/// The outputs of a task, each with how the task op or an expr op gets it.
pub(crate) fn outputs(unit: &Unit, definition: &TaskDefinition) -> Result<Vec<Output>, String> {
    let mut outputs: Vec<Output> = Vec::new();
    for decl in definition
        .output()
        .iter()
        .flat_map(|section| section.declarations())
    {
        let name = decl.name().text().to_owned();
        let ty = declared(&unit.document, &decl.ty())?;
        let r#type = convert(&ty).map_err(|why| format!("output {name}: {why}"))?;
        let expr = decl.expr();
        let uses: BTreeSet<String> = (names(&expr).into_iter())
            .filter(|used| outputs.iter().any(|item| item.name == *used))
            .collect();
        let read = match captured(&expr, &r#type) {
            Some(read) => read,
            None if reads(&expr).is_some() => {
                return Err(format!("output {name} reads files that the task makes"));
            }
            None if names(&expr).contains("task") => return Err("the task variable".into()),
            None if !has_files(&r#type) => Read::Value,
            None if placeable(&r#type) => Read::Placed,
            None => return Err(format!("output {name}: files inside this type")),
        };
        let earlier = outputs.iter().filter(|item| uses.contains(&item.name));
        if matches!(read, Read::Placed)
            && earlier
                .into_iter()
                .any(|item| matches!(item.read, Read::Glob(_) | Read::Matched(_)))
        {
            return Err(format!("output {name} refers to a captured output"));
        }
        outputs.push(Output {
            name,
            r#type,
            read,
            decl: text(&decl),
            uses,
            code: text(&expr),
        });
    }
    Ok(outputs)
}

/// How the task op reads an output like `stdout()`, `"out.txt"` or `glob("*.txt")`.
fn captured(expr: &Expr, r#type: &Type) -> Option<Read> {
    let single = matches!(single(r#type), Type::File(_) | Type::Directory);
    match expr {
        Expr::Call(call) if single && call.arguments().next().is_none() => {
            match call.target().text() {
                "stdout" => Some(Read::Glob(".wdl/stdout".into())),
                "stderr" => Some(Read::Glob(".wdl/stderr".into())),
                _ => None,
            }
        }
        Expr::Call(call) if call.target().text() == "glob" && matches!(r#type, Type::Array(_)) => {
            let mut arguments = call.arguments();
            match (
                arguments.next().as_ref().and_then(constant),
                arguments.next(),
            ) {
                (Some(Value::String(pattern)), None) if relative(&pattern) => {
                    Some(Read::Matched(pattern))
                }
                _ => None,
            }
        }
        Expr::Literal(LiteralExpr::String(_)) if single => match constant(expr) {
            Some(Value::String(path))
                if relative(&path) && !path.contains(['*', '?', '[', ']', '{', '}']) =>
            {
                Some(Read::Glob(path))
            }
            _ => None,
        },
        _ => None,
    }
}

/// The type of a value that is not null.
pub(crate) fn single(r#type: &Type) -> &Type {
    match r#type {
        Type::Optional(inner) => inner,
        other => other,
    }
}

fn relative(path: &str) -> bool {
    !path.is_empty() && !path.starts_with('/') && !path.split('/').any(|part| part == "..")
}

/// Whether the runner can place the files of a value of the type: a File, a Directory, or a
/// flat array of them without nulls. An optional array would lose `None` against `[]`.
fn placeable(r#type: &Type) -> bool {
    match r#type {
        Type::Array(array) => matches!(*array.items, Type::File(_) | Type::Directory),
        other => matches!(single(other), Type::File(_) | Type::Directory),
    }
}

/// A command section that writes `<output>\t<path>` for each file of the placed outputs, with
/// `?` before an optional output, whose files may be missing.
pub(crate) fn listing(placed: &[&Output]) -> String {
    let mut code = "command <<<\n".to_owned();
    for output in placed {
        let items = match single(&output.r#type) {
            Type::Array(_) => output.name.clone(),
            _ => format!("[{}]", output.name),
        };
        let mark = match output.r#type {
            Type::Optional(_) => "?",
            _ => "",
        };
        let line = format!(
            "~{{sep=\"\\n\" prefix(\"{mark}{}\\t\", select_all({items}))}}\n",
            output.name
        );
        code.push_str(&line);
    }
    code.push_str(">>>");
    code
}

/// The `<output>\t<regex>` lines of the outputs that `glob` calls match, for the runner.
pub(crate) fn patterns(outputs: &[Output]) -> String {
    let mut lines = String::new();
    for output in outputs {
        if let Read::Matched(pattern) = &output.read {
            lines.push_str(&format!("{}\t{}\n", output.name, expression(pattern)));
        }
    }
    lines
}

/// An extended regular expression for the paths that a glob matches, as uwe's WDL driver
/// matches them: `*` and `?` stay within a folder, `**` does not.
fn expression(pattern: &str) -> String {
    if let Some(open) = pattern.find('{')
        && let Some(close) = pattern[open..].find('}')
    {
        let (before, after) = (&pattern[..open], &pattern[open + close + 1..]);
        let choices: Vec<String> = (pattern[open + 1..open + close].split(','))
            .map(|choice| expression(&format!("{before}{choice}{after}")))
            .collect();
        return format!("({})", choices.join("|"));
    }
    let chars: Vec<char> = pattern.chars().collect();
    let mut regex = String::new();
    let mut index = 0;
    while index < chars.len() {
        match chars[index] {
            '*' if chars.get(index + 1) == Some(&'*') => {
                regex.push_str(".*");
                index += 1;
            }
            '*' => regex.push_str("[^/]*"),
            '?' => regex.push_str("[^/]"),
            '[' => match chars[index + 1..].iter().position(|c| *c == ']') {
                Some(length) => {
                    let class: String = chars[index + 1..index + 1 + length].iter().collect();
                    let class = match class.strip_prefix(['!', '^']) {
                        Some(rest) => format!("^{rest}"),
                        None => class,
                    };
                    regex.push_str(&format!("[{class}]"));
                    index += length + 1;
                }
                // A `[` without a `]` matches itself.
                None => regex.push_str("\\["),
            },
            c if "\\.^$+()|{}".contains(c) => {
                regex.push('\\');
                regex.push(c);
            }
            c => regex.push(c),
        }
        index += 1;
    }
    regex
}

#[cfg(test)]
mod tests {
    use super::expression;

    #[test]
    fn globs_become_expressions() {
        assert_eq!(expression("out/*.txt"), r"out/[^/]*\.txt");
        assert_eq!(expression("**/a?[!x]"), r".*/a[^/][^x]");
        assert_eq!(expression("*.{bam,bai}"), r"([^/]*\.bam|[^/]*\.bai)");
    }
}
