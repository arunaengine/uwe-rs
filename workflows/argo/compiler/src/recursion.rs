//! Compiles a template that calls itself, guarded by a `when`, into the body of a loop: the
//! guarded call becomes the condition of the next round and the inputs that it carries.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeSet;

use serde_json::Value as Json;
use wir::reserved::{Carry, Loop};
use wir::{ExprForm, Graph, Kind, Type};

use crate::graphs::{Compiler, Scope};
use crate::sites::{Piece, Site, call, from_available};
use crate::texts::{VALUE, text_op};
use crate::{edge, end, port};

/// The body output that says whether the template calls itself again, and the prefix of the
/// body outputs that give the inputs of that round.
pub(crate) const AGAIN: &str = "recurse";

impl Compiler {
    /// The ops of the site of `graph` that calls its own template again.
    pub(crate) fn recursion(
        &mut self,
        graph: &mut Graph,
        site: &Site,
        available: &[(String, Type)],
        scope: &Scope,
    ) -> Result<Piece, String> {
        if site.when.is_empty() {
            return Err("a recursive call without a when does not end".into());
        }
        if (graph.inputs.iter()).any(|input| input.name.starts_with("inputs.artifacts.")) {
            return Err("a recursive template takes artifacts".into());
        }
        if self.loops.contains_key(&graph.name) {
            return Err("a template calls itself twice".into());
        }
        let takes = graph.inputs.clone();
        let piece = call(site, &graph.name, &takes, &[], available, scope, None, None)?;
        let main = piece.main.clone();
        let (inner, mut ops): (Vec<_>, Vec<_>) =
            (piece.ops.into_iter()).partition(|op| op.path == main);
        let Some(inner) = inner.into_iter().next() else {
            return Err("the recursive call has no op".into());
        };
        let mut edges = Vec::new();
        let mut fed = BTreeSet::new();
        for wire in piece.edges {
            if wire.to.op.as_deref() != Some(main.as_str()) {
                edges.push(wire);
                continue;
            }
            fed.insert(wire.to.port.clone());
            edges.push(edge(
                wire.from,
                end(None, &format!("{AGAIN}.{}", wire.to.port)),
            ));
        }
        let mut carry = Vec::new();
        for input in &inner.inputs {
            let output = format!("{AGAIN}.{}", input.name);
            if !fed.contains(&input.name) {
                let declared = (takes.iter()).find(|take| take.name == input.name);
                let given = (input.default.clone()).or_else(|| declared?.default.clone());
                let Some(Json::String(value)) = given else {
                    return Err(format!("the recursive call gives no {}", input.name));
                };
                let path = format!("{main}.{}", input.name);
                let (constant, _) = text_op(&path, &value, ExprForm::Text, |_| None)?;
                ops.push(constant);
                edges.push(edge(end(Some(&path), VALUE), end(None, &output)));
            }
            graph
                .outputs
                .push(port(output.clone(), input.r#type.clone()));
            carry.push(Carry {
                output,
                input: input.name.clone(),
            });
        }
        let condition = format!("{main}.when");
        let source = from_available(available);
        let (when, wires) = text_op(&condition, &site.when, ExprForm::Condition, source)?;
        ops.push(when);
        edges.extend(wires);
        edges.push(edge(end(Some(&condition), VALUE), end(None, AGAIN)));
        graph.outputs.push(port(AGAIN, Type::Bool));
        self.loops
            .insert(graph.name.clone(), (site.name.clone(), carry));
        Ok(Piece {
            ops,
            edges,
            main: condition,
            outputs: Vec::new(),
        })
    }
}

/// The op kind of a call of the graph `called`: a loop when its template calls itself.
pub(crate) fn call_kind(called: &str, carry: Option<Vec<Carry>>) -> Kind {
    match carry {
        Some(carry) => Kind::Loop(Loop {
            body: called.to_owned(),
            condition: AGAIN.into(),
            carry,
            limit: None,
        }),
        None => Kind::Call(wir::Call {
            graph: called.to_owned(),
        }),
    }
}
