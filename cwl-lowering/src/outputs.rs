//! The call of a graph for a tool whose outputs `outputEval` computes: the ops of the tool,
//! then an expr per evaluated output over the files that its task captured.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use cwl_parser::model::{Expression, Part, Process, short_name};
use serde_json::json;
use wir::{ArrayType, Call, Expr, ExprLanguage, FileType, Flow, Graph, Kind, Port, Type};

use crate::group::{Group, SELF};
use crate::tool::Evaluated;
use crate::{Lowerer, blank, edge, process_name};

impl Lowerer {
    /// A call op at `path` of a graph that runs the ops of a tool, its task last, and an expr
    /// `<path>.<output>.outputEval` per evaluated output, which sees the tool inputs.
    pub(crate) fn evaluated(
        &mut self,
        process: &Process,
        group: Group,
        evaluated: &[Evaluated],
        library: &[String],
        path: &str,
    ) -> Result<Group, String> {
        let inputs = group.inputs();
        if inputs.iter().any(|port| port.name == SELF) {
            return Err(format!("an outputEval beside an input named {SELF}"));
        }
        let task = group.main().clone();
        let mut edges = group.edges.clone();
        for port in &inputs {
            for (op, input) in group.takers(&port.name) {
                edges.push(edge((None, &port.name), (Some(&op), &input)));
            }
        }
        let mut ops = group.ops;
        let mut outputs = Vec::new();
        for output in &process.outputs {
            let name = short_name(&output.id);
            let Some(item) = evaluated.iter().find(|item| item.output.name == name) else {
                let port = task.outputs.iter().find(|port| port.name == name);
                outputs.extend(port.cloned());
                edges.push(edge((Some(&task.path), name), (None, name)));
                continue;
            };
            let at = format!("{path}.{name}.outputEval");
            let kind = Kind::Expr(Expr {
                language: ExprLanguage::Js,
                code: unwrapped(&item.code, &item.output.r#type),
                library: library.to_vec(),
                scope: None,
                captures: Default::default(),
            });
            let mut expr = blank(&at, kind, task.origin.clone());
            expr.inputs = (inputs.iter())
                .map(|port| Port {
                    default: None,
                    ..port.clone()
                })
                .collect();
            for port in &inputs {
                edges.push(edge((None, &port.name), (Some(&at), &port.name)));
            }
            // Without a glob, `self` is an empty list, as in cwltool.
            let found = task.outputs.iter().find(|port| port.name == name);
            let mut this = Port {
                name: SELF.into(),
                r#type: found.map_or_else(files, |port| port.r#type.clone()),
                flow: Flow::Value,
                default: None,
            };
            match item.captured {
                true => edges.push(edge((Some(&task.path), name), (Some(&at), SELF))),
                false => this.default = Some(json!([])),
            }
            expr.inputs.push(this);
            expr.outputs = vec![item.output.clone()];
            edges.push(edge((Some(&at), name), (None, name)));
            outputs.push(item.output.clone());
            ops.push(expr);
        }
        let name = self.graph_name(&process_name(&process.id));
        let graph = Graph {
            name: name.clone(),
            inputs,
            outputs,
            ops,
            edges,
        };
        self.check(&graph)?;
        let mut op = blank(path, Kind::Call(Call { graph: name }), None);
        (op.inputs, op.outputs) = (graph.inputs.clone(), graph.outputs.clone());
        self.graphs.push(graph);
        Ok(Group::of(vec![op]))
    }
}

fn files() -> Type {
    Type::Array(ArrayType {
        items: Box::new(Type::File(FileType::default())),
        nonempty: false,
    })
}

/// The code of an `outputEval` whose list of no or one item becomes that item or null for an
/// output of one File or Directory, as CWL takes it.
fn unwrapped(code: &Expression, r#type: &Type) -> String {
    let single = match r#type {
        Type::Optional(inner) => matches!(**inner, Type::File(_) | Type::Directory),
        other => matches!(other, Type::File(_) | Type::Directory),
    };
    let value = match code.parts.as_slice() {
        [Part::Function(body)] => format!("(function() {{{body}}})()"),
        [Part::Javascript(code) | Part::Reference(code)] => format!("({code})"),
        _ => return code.text.clone(),
    };
    match single {
        true => format!(
            "${{ var value = {value}; if (Array.isArray(value) && value.length < 2) \
             {{ return value.length ? value[0] : null; }} return value; }}"
        ),
        false => code.text.clone(),
    }
}
