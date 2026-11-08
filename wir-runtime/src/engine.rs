//! The ops that the engine runs at once, for the run and for callback graphs: calls, branches
//! and maps start their graphs or rounds; expr, select, merge and flatten bind their value.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::path::Path;

use cwl_runtime::JavaScriptSettings;
use serde_json::{Map, Value as Json};
use wir::{Binding, Document, ExprLanguage, Kind, Status};
use workflow_graph::DriverError;

use crate::plan;
use crate::state::{Progress, widen};

/// What the engine ops of a run read besides their inputs.
pub struct Context<'a> {
    pub document: &'a Document,
    /// The folder of the document.
    pub base: &'a Path,
    pub javascript: &'a JavaScriptSettings,
    /// The text of each source that a scope names, by source id.
    pub scopes: &'a HashMap<String, String>,
    /// The file area of the run, where ops that write files keep a folder each.
    pub files: &'a Path,
    /// Where `view` prints.
    pub printer: &'a crate::driver::Printer,
    pub simulated: bool,
}

/// Runs a node of an op that the engine runs at once; whether it was such an op.
pub fn begin(
    progress: &mut Progress,
    at: usize,
    bound: &Map<String, Json>,
    context: &Context<'_>,
) -> Result<bool, DriverError> {
    let node = progress.node(at);
    let path = node.path.clone();
    let defs = &context.document.types;
    let result = match node.op.kind.clone() {
        Kind::Call(_) => {
            progress.activate(Some(at))?;
            return Ok(true);
        }
        Kind::Branch(branch) => {
            match bound.get(&branch.condition) {
                Some(Json::Bool(true)) => progress.activate(Some(at))?,
                Some(Json::Bool(false)) => {
                    let nulls = (node.op.outputs.iter())
                        .map(|port| (port.name.clone(), Json::Null))
                        .collect();
                    progress.end(at, Status::Skipped, nulls, None);
                }
                _ => {
                    let message = format!("op {path}: the condition is not a bool");
                    progress.end(at, Status::Failed, Map::new(), Some(message));
                }
            }
            return Ok(true);
        }
        Kind::Map(map) => {
            match plan::items(&map, bound) {
                Ok(rounds) => {
                    let start = progress.plan.nodes.len();
                    plan::rounds(
                        context.document,
                        &mut progress.plan.nodes,
                        at,
                        &map.body,
                        rounds.items,
                    )?;
                    progress.plan.nodes[at].shape = rounds.shape;
                    progress.adopt(start);
                }
                Err(why) => progress.end(
                    at,
                    Status::Failed,
                    Map::new(),
                    Some(format!("op {path}: {why}")),
                ),
            }
            return Ok(true);
        }
        Kind::Expr(spec) => {
            let output = node.op.outputs[0].clone();
            let value = match spec.language {
                ExprLanguage::Js | ExprLanguage::Javascript => crate::expr::evaluate(
                    &spec,
                    bound,
                    &output,
                    defs,
                    (context.javascript, context.simulated),
                ),
                ExprLanguage::Groovy | ExprLanguage::Python => language_expr(&spec, bound, context),
                ExprLanguage::Wdl => {
                    crate::wdl::expr::evaluate(context.document, &node.op, &spec, bound)
                }
            };
            value.and_then(|value| {
                let value = widen(value, &output.r#type, defs);
                match wir::fits(&value, &output.r#type, defs) {
                    true => Ok((output.name, value)),
                    false => Err(format!(
                        "the value {value} does not fit output {}",
                        output.name
                    )),
                }
            })
        }
        Kind::Select(_) | Kind::Merge(_) | Kind::Flatten(_) => {
            if (node.op.inputs.iter().chain(&node.op.outputs)).any(|port| !port.flow.is_value()) {
                let message = format!("op {path}: uwe does not run this op on streams yet");
                return Err(DriverError::unsupported(message));
            }
            let output = node.op.outputs[0].name.clone();
            crate::arrays::evaluate(&node.op, bound, defs).map(|value| (output, value))
        }
        _ => return Ok(false),
    };
    match result {
        Ok((output, value)) => progress.end(
            at,
            Status::Succeeded,
            Map::from_iter([(output, value)]),
            None,
        ),
        Err(why) => progress.end(
            at,
            Status::Failed,
            Map::new(),
            Some(format!("op {path}: {why}")),
        ),
    }
    Ok(true)
}

/// The values of the captures of a callback or expression, by name.
pub fn captured(
    captures: &std::collections::BTreeMap<String, Binding>,
    values: &Map<String, Json>,
) -> Map<String, Json> {
    (captures.iter())
        .map(|(name, binding)| {
            let value = match binding {
                Binding::Input(input) => values.get(input).cloned().unwrap_or_default(),
                Binding::Value(value) => value.clone(),
            };
            (name.clone(), value)
        })
        .collect()
}

/// The value of a Groovy or Python expression over the inputs of its op as `inputs` and its
/// captures by name; Groovy code sees the definitions of the source of its scope.
fn language_expr(
    spec: &wir::Expr,
    bound: &Map<String, Json>,
    context: &Context<'_>,
) -> Result<Json, String> {
    let mut values = captured(&spec.captures, bound);
    values.insert("inputs".to_owned(), Json::Object(bound.clone()));
    if spec.language == ExprLanguage::Python {
        let code = crate::python::Code {
            code: &spec.code,
            library: &spec.library,
        };
        return code.value(&values, context.base);
    }
    let scope = spec
        .scope
        .as_ref()
        .and_then(|scope| context.scopes.get(&scope.source));
    let names = values.keys().cloned().collect();
    let compiled =
        crate::groovy::compile(&spec.code, &spec.library, scope.map(String::as_str), names)?;
    compiled.value(&values, context.base)
}
