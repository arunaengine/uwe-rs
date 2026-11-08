//! Verifier checks of the stream operators: the fixed port names, the flows of their inputs
//! and outputs, and the item types they give.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::callbacks::Arity;
use super::{Checker, Problem};
use crate::closure::Body;
use crate::document::Port;
use crate::flow::Flow;
use crate::op::Op;
use crate::reserved::{Depth, Emit, FlatMap, Merge, NullRule, Transform};
use crate::types::{Type, resolve};

impl<'d> Checker<'d> {
    /// The input `source` and the one output `out` of an operator, with the flows `from`
    /// and `to` where they are fixed; `out` follows `source` where `to` is open.
    pub(super) fn ends<'o>(
        &mut self,
        at: &str,
        op: &'o Op,
        what: &str,
        (from, to): (Option<Flow>, Option<Flow>),
    ) -> Option<(&'o Port, &'o Port)> {
        let source = self.named_input(at, op, what, "source", from);
        let out = self.single_out(at, op, what);
        let (source, out) = (source?, out?);
        let want = to.unwrap_or(source.flow);
        if out.flow != want {
            self.report(
                at,
                Problem::Shape(what.into(), format!("a {} output", flow_name(want))),
            );
        }
        Some((source, out))
    }

    /// The input `name` of an operator, with flow `flow` where it is fixed.
    pub(super) fn named_input<'o>(
        &mut self,
        at: &str,
        op: &'o Op,
        what: &str,
        name: &str,
        flow: Option<Flow>,
    ) -> Option<&'o Port> {
        let Some(port) = op.inputs.iter().find(|port| port.name == name) else {
            self.report(
                at,
                Problem::Shape(what.into(), format!("an input named {name}")),
            );
            return None;
        };
        if let Some(flow) = flow.filter(|flow| *flow != port.flow) {
            let why = format!("a {} input {name}", flow_name(flow));
            self.report(at, Problem::Shape(what.into(), why));
        }
        Some(port)
    }

    /// The one output `out` of an operator.
    pub(super) fn single_out<'o>(&mut self, at: &str, op: &'o Op, what: &str) -> Option<&'o Port> {
        match op.outputs.as_slice() {
            [out] if out.name == "out" => Some(out),
            _ => {
                self.report(at, Problem::Shape(what.into(), "one output, out".into()));
                None
            }
        }
    }

    /// The inputs besides `fixed` are value captures.
    pub(super) fn value_captures(&mut self, at: &str, op: &Op, what: &str, fixed: &[&str]) {
        let extra = op
            .inputs
            .iter()
            .filter(|port| !fixed.contains(&port.name.as_str()));
        if extra.into_iter().any(|port| !port.flow.is_value()) {
            let why = format!("value inputs besides {}", fixed.join(" and "));
            self.report(at, Problem::Shape(what.into(), why));
        }
    }

    pub(super) fn emit(&mut self, at: &str, op: &Op, emit: &Emit) {
        let Some((source, out)) =
            self.ends(at, op, "an emit", (Some(Flow::Value), Some(Flow::Stream)))
        else {
            return;
        };
        self.value_captures(at, op, "an emit", &["source"]);
        let item = match emit.each {
            true => self.items(&source.r#type),
            false => Some(source.r#type.clone()),
        };
        if let Some(item) = item {
            self.gives(at, out, &item);
        }
    }

    pub(super) fn transform(&mut self, at: &str, op: &Op, transform: &Transform) {
        let Some((source, out)) = self.ends(at, op, "a transform", (None, None)) else {
            return;
        };
        self.value_captures(at, op, "a transform", &["source"]);
        self.body(
            at,
            op,
            &transform.body,
            (Arity::Unary, &[&source.r#type]),
            None,
        );
        let Some(made) = self.gives_type(&transform.body) else {
            return;
        };
        // A dropped null leaves the items that are set.
        let made = match (transform.nulls, resolve(made, self.defs)) {
            (NullRule::Drop, Some(Type::Optional(inner))) => (**inner).clone(),
            _ => made.clone(),
        };
        self.gives(at, out, &made);
    }

    pub(super) fn filter(&mut self, at: &str, op: &Op, body: &Body) {
        let Some((source, out)) = self.ends(at, op, "a filter", (None, None)) else {
            return;
        };
        self.value_captures(at, op, "a filter", &["source"]);
        self.body(
            at,
            op,
            body,
            (Arity::Unary, &[&source.r#type]),
            Some(&Type::Bool),
        );
        self.gives(at, out, &source.r#type);
    }

    pub(super) fn flat_map(&mut self, at: &str, op: &Op, flat: &FlatMap) {
        let stream = Some(Flow::Stream);
        let Some((source, _)) = self.ends(at, op, "a flat_map", (stream, stream)) else {
            return;
        };
        self.value_captures(at, op, "a flat_map", &["source"]);
        if let Some(body) = &flat.body {
            self.body(at, op, body, (Arity::Unary, &[&source.r#type]), None);
        }
    }

    pub(super) fn flatten_stream(&mut self, at: &str, op: &Op, depth: Depth) {
        let stream = Some(Flow::Stream);
        let Some((source, out)) = self.ends(at, op, "a flatten", (stream, stream)) else {
            return;
        };
        self.value_captures(at, op, "a flatten", &["source"]);
        if depth == Depth::One
            && let Some(items) = self.items(&source.r#type)
        {
            self.gives(at, out, &items);
        }
    }

    /// The item type of an array type, or dynamic for a dynamic type.
    pub(super) fn items(&self, r#type: &Type) -> Option<Type> {
        match resolve(r#type, self.defs)? {
            Type::Array(array) => Some((*array.items).clone()),
            Type::Dynamic => Some(Type::Dynamic),
            _ => None,
        }
    }
}

fn flow_name(flow: Flow) -> &'static str {
    match flow {
        Flow::Value => "value",
        Flow::Stream => "stream",
    }
}

impl<'d> Checker<'d> {
    /// Two or more stream inputs, and one stream output `out` that accepts each of them.
    pub(super) fn streams_of(&mut self, at: &str, op: &Op, what: &str) {
        if op.inputs.len() < 2 || op.inputs.iter().any(|port| port.flow.is_value()) {
            self.report(
                at,
                Problem::Shape(what.into(), "two or more stream inputs".into()),
            );
        }
        let Some(out) = self.single_out(at, op, what) else {
            return;
        };
        if out.flow.is_value() {
            self.report(at, Problem::Shape(what.into(), "a stream output".into()));
        }
        for input in &op.inputs {
            self.gives(at, out, &input.r#type);
        }
    }

    /// Takes rounds: a stream output when the first input is a stream, else a value; the
    /// body sees the inputs by name.
    pub(super) fn merge_rounds(&mut self, at: &str, op: &Op, merge: &Merge) {
        let Some(out) = self.single_out(at, op, "a merge") else {
            return;
        };
        let Some(first) = op.inputs.first() else {
            self.report(at, Problem::Shape("a merge".into(), "an input".into()));
            return;
        };
        if out.flow != first.flow {
            let why = format!("a {} output, as its first input", flow_name(first.flow));
            self.report(at, Problem::Shape("a merge".into(), why));
        }
        if let Some(body) = &merge.body {
            self.body(at, op, body, (Arity::Ports, &[]), None);
        }
    }

    /// Two or more streams, and a stream of tuples of their items, which take null where
    /// a `remainder` pads them.
    pub(super) fn zip(&mut self, at: &str, op: &Op, remainder: bool) {
        self.streams_of_tuples(at, op);
        let Some(out) = op.outputs.first() else {
            return;
        };
        let items = op.inputs.iter().map(|port| match remainder {
            true => Type::Optional(Box::new(port.r#type.clone())),
            false => port.r#type.clone(),
        });
        self.gives(at, out, &Type::Tuple(items.collect()));
    }

    fn streams_of_tuples(&mut self, at: &str, op: &Op) {
        if op.inputs.len() < 2 || op.inputs.iter().any(|port| port.flow.is_value()) {
            self.report(
                at,
                Problem::Shape("a zip".into(), "two or more stream inputs".into()),
            );
        }
        if let Some(out) = self.single_out(at, op, "a zip")
            && out.flow.is_value()
        {
            self.report(at, Problem::Shape("a zip".into(), "a stream output".into()));
        }
    }
}
