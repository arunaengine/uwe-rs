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
            Some(pattern) => Read::Glob(pattern),
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
                .any(|item| matches!(item.read, Read::Glob(_)))
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

/// The glob that captures an output like `stdout()`, `"out.txt"` or `glob("*.txt")`.
fn captured(expr: &Expr, r#type: &Type) -> Option<String> {
    let single = matches!(single(r#type), Type::File(_) | Type::Directory);
    match expr {
        Expr::Call(call) if single && call.arguments().next().is_none() => {
            match call.target().text() {
                "stdout" => Some(".wdl/stdout".into()),
                "stderr" => Some(".wdl/stderr".into()),
                _ => None,
            }
        }
        Expr::Call(call) if call.target().text() == "glob" && matches!(r#type, Type::Array(_)) => {
            let mut arguments = call.arguments();
            match (
                arguments.next().as_ref().and_then(constant),
                arguments.next(),
            ) {
                (Some(Value::String(pattern)), None) if relative(&pattern) => Some(pattern),
                _ => None,
            }
        }
        Expr::Literal(LiteralExpr::String(_)) if single => match constant(expr) {
            Some(Value::String(path))
                if relative(&path) && !path.contains(['*', '?', '[', ']', '{', '}']) =>
            {
                Some(path)
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
/// flat array of them without nulls.
fn placeable(r#type: &Type) -> bool {
    match single(r#type) {
        Type::File(_) | Type::Directory => true,
        Type::Array(array) => matches!(*array.items, Type::File(_) | Type::Directory),
        _ => false,
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
