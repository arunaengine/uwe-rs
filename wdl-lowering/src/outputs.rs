//! The outputs of a WDL task, which expr ops compute; outputs with files have no lowering yet.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeSet;

use wdl_ast::AstToken;
use wdl_ast::v1::TaskDefinition;
use wdl_runtime::document::{Unit, declared};
use wdl_runtime::expression::names;
use wir::Type;

use crate::types::{convert, has_files};
use crate::{reads, text};

pub(crate) struct Output {
    pub name: String,
    pub r#type: Type,
    /// The outputs it refers to, and its expression as WDL text.
    pub uses: BTreeSet<String>,
    pub code: String,
}

/// The outputs of a task, which expr ops compute from the inputs and other outputs.
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
        if reads(&expr).is_some() {
            return Err(format!("output {name} reads files that the task makes"));
        }
        if names(&expr).contains("task") {
            return Err("the task variable".into());
        }
        if has_files(&r#type) {
            return Err(format!("output {name} has files"));
        }
        let uses: BTreeSet<String> = (names(&expr).into_iter())
            .filter(|used| outputs.iter().any(|item| item.name == *used))
            .collect();
        outputs.push(Output {
            name,
            r#type,
            uses,
            code: text(&expr),
        });
    }
    Ok(outputs)
}
