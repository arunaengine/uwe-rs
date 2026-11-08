//! The library of the expr ops of a WDL task: the declarations that compute input defaults
//! and the private declarations, in an order where each comes after the ones it uses.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, BTreeSet};

use wdl_analysis::types::Optional;

use wdl_ast::v1::{BoundDecl, Decl, Expr, LiteralString, TaskDefinition};
use wdl_ast::{AstNode, AstToken};
use wdl_runtime::Value;
use wdl_runtime::document::{Unit, declared};
use wdl_runtime::expression::names;
use wir::{KeyType, Port, Type};

use crate::types::{convert, has_files, optional, port};
use crate::{constant, flag, presence, reads, text, unclaimed};

/// The inputs of a task as graph ports, with the flags of given inputs, and the declarations
/// that compute their defaults.
pub(crate) struct Inputs {
    pub ports: Vec<Port>,
    pub library: Vec<Line>,
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
    let mut flags = Vec::new();
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
        unclaimed(&name)?;
        let mut item = port(&name, r#type.clone());
        if let Decl::Bound(bound) = &decl {
            let expr = bound.expr();
            check(&expr)?;
            match constant(&expr).map(|value| value.coerce(&ty)) {
                Some(Ok(Value::None)) => item.r#type = optional(r#type),
                Some(Ok(value)) if !ty.is_optional() => item.default = Some(value.result_json()),
                _ => {
                    item.r#type = optional(r#type);
                    // A given `None` keeps an optional input from its default.
                    let given = match ty.is_optional() {
                        true => {
                            flags.push(flag(&name));
                            presence(&name)
                        }
                        false => format!("defined({name})"),
                    };
                    let code = format!(
                        "{} {name} = if {given} then {name} else ({})",
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
    inputs.extend(flags);
    Ok(Inputs {
        ports: inputs,
        library,
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

/// Fails for a declaration that makes files from text, like `File f = path` for a String
/// `path`: uwe's WDL driver stages those files, while a task op stages only its inputs.
pub(crate) fn staged(decl: &BoundDecl, types: &BTreeMap<String, Type>) -> Result<(), String> {
    let name = decl.name().text().to_owned();
    if !types.get(&name).is_some_and(has_files) {
        return Ok(());
    }
    let literal = decl.descendants::<LiteralString>().next().is_some();
    let uses = names(&decl.expr());
    match literal || uses.iter().any(|used| types.get(used).is_none_or(textual)) {
        true => Err(format!("the declaration {name} makes files from text")),
        false => Ok(()),
    }
}

/// Whether a value of the type can hold text, which an expression can turn into a path.
fn textual(r#type: &Type) -> bool {
    match r#type {
        Type::String | Type::Enum(_) | Type::Dynamic => true,
        Type::Optional(inner) => textual(inner),
        Type::Array(array) => textual(&array.items),
        Type::Map(map) => map.key == KeyType::String || textual(&map.value),
        Type::Record(fields) => fields.iter().any(|field| textual(&field.r#type)),
        Type::Union(items) | Type::Tuple(items) => items.iter().any(textual),
        _ => false,
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
