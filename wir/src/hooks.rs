//! Verifier checks of run hooks: closures that take one object, captures of the inputs of
//! the entry graph, and observers of an existing source with distinct process ids.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeSet;

use super::{Checker, Problem};
use crate::closure::{Binding, Closure, ExprLanguage};
use crate::document::Hook;

impl Checker<'_> {
    pub(super) fn hooks(&mut self) {
        let document = self.document;
        for (index, hook) in document.hooks.iter().enumerate() {
            let at = format!("hook {index}");
            match hook {
                Hook::Complete(closure) | Hook::Error(closure) => self.hook_closure(&at, closure),
                Hook::Observe(observers) => {
                    self.source(&at, &observers.source);
                    let ids: BTreeSet<u32> =
                        observers.processes.iter().map(|item| item.id).collect();
                    if ids.len() < observers.processes.len() {
                        self.report(&at, Problem::Duplicate("a process id".into()));
                    }
                    let mut feeds = observers.processes.iter().flat_map(|item| &item.feeds);
                    if let Some(unknown) = feeds.find(|id| !ids.contains(id)) {
                        let why = format!("feeds of known processes, not {unknown}");
                        self.report(&at, Problem::Shape("an observer".into(), why));
                    }
                }
            }
        }
    }

    /// A hook closure takes one object; its captures name inputs of the entry graph, or value
    /// outputs of its ops.
    fn hook_closure(&mut self, at: &str, closure: &Closure) {
        let wrong = match (closure.language, closure.parameters.len()) {
            (ExprLanguage::Js | ExprLanguage::Argo, _) => Some("a closure language"),
            (ExprLanguage::Wdl, 1) | (ExprLanguage::Wdl, 0) => None,
            (ExprLanguage::Wdl, _) => Some("a wdl closure that names one parameter"),
            (_, 0) => None,
            _ => Some("no parameter names outside wdl"),
        };
        if let Some(why) = wrong {
            self.report(at, Problem::Shape("a hook".into(), why.into()));
        }
        let document = self.document;
        let entry = document
            .graphs
            .iter()
            .find(|graph| graph.name == document.entry);
        for (name, binding) in &closure.captures {
            let why = match binding {
                Binding::Input(input) => (!entry
                    .is_some_and(|graph| graph.inputs.iter().any(|port| port.name == *input)))
                .then(|| format!("capture {name} names no input {input} of the entry graph")),
                Binding::Output(end) => {
                    let op = entry.and_then(|graph| {
                        (graph.ops.iter()).find(|op| end.op.as_deref() == Some(op.path.as_str()))
                    });
                    let port = op.and_then(|op| op.outputs.iter().find(|p| p.name == end.port));
                    (!port.is_some_and(|port| port.flow.is_value())).then(|| {
                        format!("capture {name} names no value output of an op of the entry graph")
                    })
                }
                Binding::Value(_) => None,
            };
            if let Some(why) = why {
                self.report(at, Problem::Closure(why));
            }
        }
        if let Some(scope) = &closure.scope {
            self.scope(at, scope, closure.language);
        }
    }
}
