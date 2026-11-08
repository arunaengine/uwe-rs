//! The library of the expr ops of a WDL task: the declarations that compute input defaults
//! and the private declarations, in an order where each comes after the ones it uses.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeSet;

use serde_json::Value as Json;
use wdl_analysis::types::Optional;

use wdl_ast::AstToken;
use wdl_ast::v1::{Decl, Expr, TaskDefinition};
use wdl_runtime::Value;
use wdl_runtime::document::{Unit, declared};
use wdl_runtime::expression::names;
use wir::{Port, Type};

use crate::types::{convert, has_files, optional, port};
use crate::{constant, reads, text};

/// The inputs of a task as graph ports, the declarations that compute their defaults, and
/// the literal defaults of optional inputs, which the call ops hold.
pub(crate) struct Inputs {
    pub ports: Vec<Port>,
    pub library: Vec<Line>,
    pub fallbacks: Vec<(String, Json)>,
}

/// A declaration as WDL text, with its name and the names it uses.
pub(crate) struct Line {
    pub name: String,
    pub uses: BTreeSet<String>,
    pub code: String,
}

/// The inputs of a task as graph ports, a literal default as the default of its port, and
/// the declarations that compute the other defaults.
pub(crate) fn ports(unit: &Unit, definition: &TaskDefinition) -> Result<Inputs, String> {
    let mut inputs = Vec::new();
    let mut library = Vec::new();
    let mut fallbacks = Vec::new();
    for decl in definition
        .input()
        .iter()
        .flat_map(|section| section.declarations())
    {
        let name = decl.name().text().to_owned();
        if decl.env().is_some() {
            return Err("an env declaration".into());
        }
        let ty = declared(&unit.document, &decl.ty())?;
        let r#type = convert(&ty).map_err(|why| format!("input {name}: {why}"))?;
        if nested(&r#type) {
            return Err(format!("input {name}: a file inside a struct or map"));
        }
        let mut item = port(&name, r#type.clone());
        if let Decl::Bound(bound) = &decl {
            let expr = bound.expr();
            check(&expr)?;
            match constant(&expr).map(|value| value.coerce(&ty)) {
                Some(Ok(Value::None)) => item.r#type = optional(r#type),
                Some(Ok(value)) if ty.is_optional() => {
                    fallbacks.push((name.clone(), value.result_json()))
                }
                Some(Ok(value)) => item.default = Some(value.result_json()),
                _ => {
                    item.r#type = optional(r#type);
                    let code = format!(
                        "{} {name} = if defined({name}) then {name} else ({})",
                        decl.ty().inner().text(),
                        text(&expr)
                    );
                    let mut uses = names(&expr);
                    uses.remove(&name);
                    library.push(Line {
                        name: name.clone(),
                        uses,
                        code,
                    });
                }
            }
        }
        inputs.push(item);
    }
    Ok(Inputs {
        ports: inputs,
        library,
        fallbacks,
    })
}

/// Whether a type holds files inside a record or a map, which a task op does not stage.
fn nested(r#type: &Type) -> bool {
    match r#type {
        Type::Optional(inner) => nested(inner),
        Type::Array(array) => nested(&array.items),
        Type::Tuple(items) => items.iter().any(nested),
        Type::Record(_) | Type::Map(_) => has_files(r#type),
        _ => false,
    }
}

/// Fails for an expression that reads or writes files or uses the task variable.
pub(crate) fn check(expr: &Expr) -> Result<(), String> {
    if let Some(function) = reads(expr) {
        return Err(format!("{function} in a declaration"));
    }
    match names(expr).contains("task") {
        true => Err("the task variable".into()),
        false => Ok(()),
    }
}

/// The declarations in an order where each comes after the ones it uses.
pub(crate) fn ordered(mut pending: Vec<Line>) -> Result<Vec<String>, String> {
    let declared: BTreeSet<String> = pending.iter().map(|line| line.name.clone()).collect();
    let mut done: BTreeSet<String> = BTreeSet::new();
    let mut lines = Vec::new();
    while !pending.is_empty() {
        let ready = pending.iter().position(|line| {
            line.uses
                .iter()
                .all(|name| !declared.contains(name) || done.contains(name))
        });
        let Some(ready) = ready else {
            return Err("declarations that refer to each other".into());
        };
        let line = pending.remove(ready);
        done.insert(line.name);
        lines.push(line.code);
    }
    Ok(lines)
}
