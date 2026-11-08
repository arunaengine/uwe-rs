//! Lowers a CWL ExpressionTool into a call of a graph with one expr op per output, each of
//! which takes its field of the object that the expression gives.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use cwl_parser::model::{ExpressionTool, Part, Process, Type as Cwl};
use wir::{Call, Edge, Expr, Graph, Kind, Op, Port};

use crate::workflow::end;
use crate::{Lowerer, Scope, blank, boundary, defaults, process_name, requirements, types};

impl Lowerer {
    /// A call op at `path` of a graph whose exprs compute the outputs of an ExpressionTool.
    pub(crate) fn expression(
        &mut self,
        process: &Process,
        tool: &ExpressionTool,
        scope: &Scope,
        path: &str,
    ) -> Result<Op, String> {
        if let Some(entry) =
            (scope.requirements.iter()).find(|entry| !requirements::known(&entry.class))
        {
            return Err(format!(
                "the requirement {}",
                requirements::name(&entry.class)
            ));
        }
        let value = match tool.expression.parts.as_slice() {
            [Part::Function(body)] => format!("(function() {{{body}}})()"),
            [Part::Javascript(code) | Part::Reference(code)] => format!("({code})"),
            _ => return Err("an expression that is not one $(...) or ${...}".into()),
        };
        let library = scope.library();
        let (mut inputs, outputs) = boundary(process, scope)?;
        let named = types::named(scope);
        for (port, input) in inputs.iter_mut().zip(&process.inputs) {
            let name = &port.name;
            // An expr reads no files, so it cannot load contents or listings.
            let loads = input.load_contents == Some(true)
                || (input.binding.as_ref())
                    .is_some_and(|binding| binding.load_contents == Some(true));
            if loads || input.load_listing.is_some() {
                return Err(format!("loadContents or loadListing of input {name}"));
            }
            // wir takes `dynamic` as optional, where CWL needs a value for a required `Any`.
            if input.value_type == Cwl::Any && input.default.is_none() {
                return Err(format!("the Any input {name}"));
            }
            if !input.secondary_files.is_empty() || !input.format.is_empty() {
                return Err(format!("the secondary files or format of input {name}"));
            }
            if let Some(default) = &input.default {
                port.r#type = types::convert(&input.value_type, &named)?;
                defaults::set(port, default, &self.base)
                    .map_err(|why| format!("input {}: {why}", port.name))?;
            }
        }
        let name = self.graph_name(&process_name(&process.id));
        let mut ops = Vec::new();
        let mut edges = Vec::new();
        for output in &outputs {
            let field = serde_json::to_string(&output.name).unwrap_or_default();
            let code = format!(
                "${{ var value = {value}[{field}]; return value === undefined ? null : value; }}"
            );
            let kind = Kind::Expr(Expr {
                language: "js".into(),
                code,
                library: library.clone(),
            });
            let mut op = blank(&output.name, kind, None);
            // The graph applies the defaults before its exprs see the inputs.
            op.inputs = (inputs.iter())
                .map(|port| Port {
                    default: None,
                    ..port.clone()
                })
                .collect();
            let mut out = output.clone();
            out.name = "out".into();
            op.outputs = vec![out];
            for port in &inputs {
                edges.push(Edge {
                    from: end(None, &port.name),
                    to: end(Some(&output.name), &port.name),
                });
            }
            edges.push(Edge {
                from: end(Some(&output.name), "out"),
                to: end(None, &output.name),
            });
            ops.push(op);
        }
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
        Ok(op)
    }
}
