//! Verifier checks of the ops with effects: collect_file, view and observe, and the effects
//! that file and print work declares.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::callbacks::Arity;
use super::selection::places;
use super::{Checker, Problem};
use crate::op::{Effect, Kind, Op};
use crate::reserved::{CollectFile, Observe, View};
use crate::types::{FileType, Type};

impl<'d> Checker<'d> {
    pub(super) fn collect_file(&mut self, at: &str, op: &Op, collect: &CollectFile) {
        let Some((source, out)) = self.streamed(at, op, "a collect_file") else {
            return;
        };
        if let Some(body) = &collect.body {
            self.body(at, op, body, (Arity::Unary, &[&source.r#type]), None);
        }
        self.sort(at, op, &collect.sort);
        self.gives(at, out, &Type::File(FileType::default()));
        let wrong = places(op, collect.name.as_deref(), collect.directory.as_deref());
        for why in wrong {
            self.report(at, Problem::Shape("a collect_file".into(), why));
        }
    }

    pub(super) fn view(&mut self, at: &str, op: &Op, view: &View) {
        let Some((source, out)) = self.ends(at, op, "a view", (None, None)) else {
            return;
        };
        self.value_captures(at, op, "a view", &["source"]);
        if let Some(body) = &view.body {
            self.body(at, op, body, (Arity::Unary, &[&source.r#type]), None);
        }
        self.gives(at, out, &source.r#type);
    }

    pub(super) fn observe(&mut self, at: &str, op: &Op, observe: &Observe) {
        let Some((source, out)) = self.streamed(at, op, "an observe") else {
            return;
        };
        let callbacks = [
            (&observe.item, Arity::Unary),
            (&observe.close, Arity::Zero),
            (&observe.error, Arity::Unary),
        ];
        for (closure, arity) in callbacks {
            if let Some(closure) = closure {
                self.closure(at, op, closure, arity);
            }
        }
        self.gives(at, out, &source.r#type);
    }

    /// The op declares the effects its own work has: printing, writing and reading files.
    pub(super) fn declared_effects(&mut self, at: &str, op: &Op) {
        let reads = |op: &Op| {
            let source = op.inputs.iter().find(|port| port.name == "source");
            source.is_some_and(|port| matches!(port.r#type, Type::File(_)) && !port.flow.is_value())
        };
        let needed = match &op.kind {
            Kind::View(_) => vec![Effect::Log],
            Kind::CollectFile(_) => vec![Effect::WriteFiles],
            Kind::Split(split) => {
                let writes = split.files.as_ref().map(|_| Effect::WriteFiles);
                let read = reads(op).then_some(Effect::ReadFiles);
                writes.into_iter().chain(read).collect()
            }
            _ => Vec::new(),
        };
        for effect in needed
            .into_iter()
            .filter(|effect| !op.effects.contains(effect))
        {
            let name = serde_json::to_string(&effect).unwrap_or_default();
            self.report(
                at,
                Problem::Shape(
                    format!("op kind {}", kind_name(op)),
                    format!("the effect {name}"),
                ),
            );
        }
    }
}

fn kind_name(op: &Op) -> String {
    let value = serde_json::to_value(&op.kind).unwrap_or_default();
    (value
        .as_object()
        .and_then(|object| object.keys().next().cloned()))
    .unwrap_or_default()
}
