//! Verifier checks of the fields of each op kind: task captures, call ports, native boundaries.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeSet;

use super::{Checker, Problem, show, walk};
use crate::op::{Argument, CaptureRule, Kind, Native, Op, Task};
use crate::types::{Type, TypeDef, resolve};

impl<'d> Checker<'d> {
    pub(super) fn kind(&mut self, at: &str, op: &Op) {
        match &op.kind {
            Kind::Task(task) => self.task(at, op, task),
            Kind::Call(call) => self.call(at, op, &call.graph),
            Kind::Native(native) => self.native(at, op, native),
            Kind::Map(map) => self.inputs(at, op, map.over.iter()),
            Kind::Branch(branch) => self.inputs(at, op, [&branch.condition]),
            Kind::Dynamic(dynamic) => self.inputs(at, op, [&dynamic.plan]),
            _ => {}
        }
    }

    fn task(&mut self, at: &str, op: &Op, task: &Task) {
        if task.command.is_empty() {
            self.report(at, Problem::Empty("command".into()));
        }
        let used = task.command.iter().filter_map(|argument| match argument {
            Argument::Input { input } => Some(input),
            Argument::Text(_) => None,
        });
        self.inputs(at, op, used);
        for capture in &task.captures {
            if !op.outputs.iter().any(|port| port.name == capture.output) {
                self.report(at, Problem::NoPort(format!("output {}", capture.output)));
            }
        }
        for output in &op.outputs {
            let mut rules = task
                .captures
                .iter()
                .filter(|item| item.output == output.name);
            let (Some(capture), None) = (rules.next(), rules.next()) else {
                self.report(at, Problem::Capture(output.name.clone()));
                continue;
            };
            if !capture_fits(&capture.from, &output.r#type, self.defs) {
                self.report(at, Problem::CaptureType(output.name.clone()));
            }
        }
    }

    /// The op's ports must be the called graph's ports, with types each side accepts.
    fn call(&mut self, at: &str, op: &Op, name: &str) {
        let Some(graph) = self.graph(name) else {
            return;
        };
        let sides = [
            ("input", &op.inputs, &graph.inputs),
            ("output", &op.outputs, &graph.outputs),
        ];
        for (side, ours, theirs) in sides {
            let names: BTreeSet<&str> =
                ours.iter().chain(theirs).map(|p| p.name.as_str()).collect();
            for port in names {
                let mine = ours.iter().find(|item| item.name == port);
                let other = theirs.iter().find(|item| item.name == port);
                let fits = match (side, mine, other) {
                    ("input", Some(mine), Some(other)) => {
                        other.r#type.accepts(&mine.r#type, self.defs)
                    }
                    (_, Some(mine), Some(other)) => mine.r#type.accepts(&other.r#type, self.defs),
                    _ => false,
                };
                if !fits || mine.map(|item| item.flow) != other.map(|item| item.flow) {
                    let port = format!("{side} {port}");
                    self.report(
                        at,
                        Problem::CallPort {
                            port,
                            graph: name.to_owned(),
                        },
                    );
                }
            }
        }
    }

    fn inputs<'n>(&mut self, at: &str, op: &Op, names: impl IntoIterator<Item = &'n String>) {
        for name in names {
            if !op.inputs.iter().any(|port| port.name == *name) {
                self.report(at, Problem::NoPort(format!("input {name}")));
            }
        }
    }

    fn native(&mut self, at: &str, op: &Op, native: &Native) {
        if let Some(source) = self.source(at, &native.source)
            && source.language != native.language
        {
            let named = native.source.clone();
            let (found, want) = (show(&native.language), show(&source.language));
            self.report(at, Problem::Language { named, found, want });
        }
        for port in op.inputs.iter().chain(&op.outputs) {
            if self.has_dynamic(&port.r#type) {
                self.report(at, Problem::Untyped(format!("port {}", port.name)));
            }
        }
    }

    fn has_dynamic(&self, r#type: &Type) -> bool {
        let mut found = false;
        walk(r#type, &mut |item| match item {
            Type::Dynamic => found = true,
            Type::Named(name) => found |= self.named(name).is_some_and(|t| self.has_dynamic(t)),
            _ => {}
        });
        found
    }
}

/// Glob captures fill File, Directory or arrays of them; stdout and stderr fill a File.
fn capture_fits(rule: &CaptureRule, r#type: &Type, defs: &[TypeDef]) -> bool {
    let Some(r#type) = resolve(r#type, defs) else {
        return false;
    };
    match (rule, r#type) {
        (_, Type::Optional(inner)) => capture_fits(rule, inner, defs),
        (_, Type::File(_) | Type::Dynamic) => true,
        (CaptureRule::Glob(_), Type::Directory) => true,
        (CaptureRule::Glob(_), Type::Array(array)) => {
            matches!(
                resolve(&array.items, defs),
                Some(Type::File(_) | Type::Directory)
            )
        }
        _ => false,
    }
}
