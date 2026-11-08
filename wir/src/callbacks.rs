//! Verifier checks of callbacks: the ports and purity of callback graphs, and the language,
//! parameters, scope and captures of closures and expressions.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use super::{Checker, Problem, show};
use crate::closure::{Binding, Body, Closure, ExprLanguage, Scope};
use crate::document::{Graph, Language};
use crate::op::{Kind, Op};
use crate::types::Type;

/// The positional arguments of a callback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Arity {
    /// `item`.
    Unary,
    /// `left` and `right`.
    Binary,
    /// The inputs of the op by name, an item of each stream input and each value input.
    Ports,
}

impl Arity {
    fn names(self) -> &'static [&'static str] {
        match self {
            Arity::Unary => &["item"],
            Arity::Binary => &["left", "right"],
            Arity::Ports => &[],
        }
    }
}

impl<'d> Checker<'d> {
    /// Checks a callback of `op` with `arity`, whose positional arguments have the types
    /// `args` where known; `returns` is the type its result must have.
    pub(super) fn body(
        &mut self,
        at: &str,
        op: &Op,
        body: &Body,
        (arity, args): (Arity, &[&Type]),
        returns: Option<&Type>,
    ) {
        match body {
            Body::Graph(name) => self.callback(at, op, name, (arity, args), returns),
            Body::Closure(closure) => self.closure(at, op, closure, arity),
        }
    }

    /// The type of the output `out` of a callback graph.
    pub(super) fn gives_type(&self, body: &Body) -> Option<&'d Type> {
        let graph = self.graph(body.graph()?)?;
        let out = graph.outputs.iter().find(|port| port.name == "out")?;
        Some(&out.r#type)
    }

    fn callback(
        &mut self,
        at: &str,
        op: &Op,
        name: &str,
        (arity, args): (Arity, &[&Type]),
        returns: Option<&Type>,
    ) {
        // The op check reports a body graph that does not exist.
        let Some(graph) = self.graph(name) else {
            return;
        };
        let mut why = Vec::new();
        let ports = graph.inputs.iter().chain(&graph.outputs);
        if ports.into_iter().any(|port| !port.flow.is_value()) {
            why.push("a callback graph has value ports only".to_owned());
        }
        match graph.outputs.as_slice() {
            [out] if out.name == "out" => {
                if let Some(want) = returns
                    && !want.accepts(&out.r#type, self.defs)
                {
                    why.push(format!(
                        "output out gives {} but {} is wanted",
                        show(&out.r#type),
                        show(want)
                    ));
                }
            }
            _ => why.push("a callback graph has one output, out".to_owned()),
        }
        for (index, position) in arity.names().iter().enumerate() {
            match graph.inputs.iter().find(|port| port.name == *position) {
                None => why.push(format!("input {position} is missing")),
                Some(port) => {
                    if let Some(arg) = args.get(index)
                        && !port.takes().accepts(arg, self.defs)
                    {
                        why.push(format!("input {position} does not take {}", show(*arg)));
                    }
                }
            }
        }
        for port in graph
            .inputs
            .iter()
            .filter(|port| !arity.names().contains(&port.name.as_str()))
        {
            let captured = op.inputs.iter().find(|item| item.name == port.name);
            let ports = arity == Arity::Ports;
            match captured {
                Some(item) if item.flow.is_value() || ports => {
                    if !port.takes().accepts(&item.r#type, self.defs) {
                        why.push(format!(
                            "capture {} does not take input {}",
                            port.name, item.name
                        ));
                    }
                }
                _ => why.push(format!(
                    "capture {} names no value input of the op",
                    port.name
                )),
            }
        }
        if let Some(impure) = self.impure(graph) {
            why.push(format!("it runs {impure}, which is not pure"));
        }
        for problem in why {
            self.report(at, Problem::Callback(name.to_owned(), problem));
        }
    }

    /// The first op below a graph, through calls and bodies, that is not pure.
    fn impure(&self, graph: &Graph) -> Option<String> {
        let mut reached = vec![graph.name.as_str()];
        let mut next = 0;
        while let Some(name) = reached.get(next).copied() {
            next += 1;
            let Some(graph) = self.graph(name) else {
                continue;
            };
            for op in &graph.ops {
                let effectful = matches!(
                    op.kind,
                    Kind::Task(_) | Kind::Native(_) | Kind::Dynamic(_) | Kind::Loop(_)
                );
                if effectful || !op.effects.is_empty() {
                    return Some(format!("{name}/{}", op.path));
                }
                for body in super::called_by(&op.kind) {
                    if !reached.contains(&body) {
                        reached.push(body);
                    }
                }
            }
        }
        None
    }

    fn closure(&mut self, at: &str, op: &Op, closure: &Closure, arity: Arity) {
        let mut why = Vec::new();
        match (closure.language, closure.parameters.len()) {
            (ExprLanguage::Js, _) => why.push("js is no closure language; use javascript".into()),
            (ExprLanguage::Wdl, count) if count != arity.names().len() => {
                why.push(format!(
                    "a wdl closure names {} parameters",
                    arity.names().len()
                ));
            }
            (ExprLanguage::Wdl, _) | (_, 0) => {}
            _ => why.push("only a wdl closure names its parameters".into()),
        }
        for name in closure
            .captures
            .keys()
            .filter(|name| closure.parameters.contains(name))
        {
            why.push(format!("capture {name} is also a parameter"));
        }
        for problem in why {
            self.report(at, Problem::Closure(problem));
        }
        self.captures(at, op, &closure.captures);
        if let Some(scope) = &closure.scope {
            self.scope(at, scope, closure.language);
        }
    }

    /// The captures name value inputs of the op.
    pub(super) fn captures(&mut self, at: &str, op: &Op, captures: &BTreeMap<String, Binding>) {
        for (name, binding) in captures {
            let Binding::Input(input) = binding else {
                continue;
            };
            let found = op.inputs.iter().find(|port| port.name == *input);
            if !found.is_some_and(|port| port.flow.is_value()) {
                let why = format!("capture {name} names no value input {input}");
                self.report(at, Problem::Closure(why));
            }
        }
    }

    /// The scope is a byte range of a source in the language of the code.
    pub(super) fn scope(&mut self, at: &str, scope: &Scope, language: ExprLanguage) {
        if scope.start > scope.end {
            let why = format!(
                "the scope {}..{} ends before it starts",
                scope.start, scope.end
            );
            self.report(at, Problem::Closure(why));
        }
        let Some(source) = self.source(at, &scope.source) else {
            return;
        };
        let want = match language {
            ExprLanguage::Groovy => Language::Nextflow,
            ExprLanguage::Python => Language::Snakemake,
            ExprLanguage::Wdl => Language::Wdl,
            ExprLanguage::Js | ExprLanguage::Javascript => Language::Cwl,
        };
        if source.language != want {
            let named = scope.source.clone();
            let (found, want) = (show(&language), show(&source.language));
            self.report(at, Problem::Language { named, found, want });
        }
    }
}
