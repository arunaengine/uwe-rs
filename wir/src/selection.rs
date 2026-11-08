//! Verifier checks of the ops that select or cut the items of a stream: buffer, window,
//! take, unique, distinct, until and if_empty.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::callbacks::Arity;
use super::kinds::array_of;
use super::{Checker, Problem};
use crate::closure::Body;
use crate::document::Port;
use crate::flow::Flow;
use crate::op::Op;
use crate::reserved::{Buffer, BufferRule, IfEmpty, Window};
use crate::types::Type;

impl<'d> Checker<'d> {
    pub(super) fn buffer(&mut self, at: &str, op: &Op, buffer: &Buffer) {
        let Some((source, out)) = self.streamed(at, op, "a buffer") else {
            return;
        };
        let item = [&source.r#type];
        match &buffer.rule {
            BufferRule::Size(rule) if rule.size == 0 => {
                self.report(
                    at,
                    Problem::Shape("a buffer".into(), "a positive size".into()),
                );
            }
            BufferRule::Size(_) => {}
            BufferRule::Match(rule) => {
                for body in rule.open.iter().chain([&rule.close]) {
                    self.body(at, op, body, (Arity::Unary, &item), Some(&Type::Bool));
                }
            }
        }
        self.gives(at, out, &array_of(source.r#type.clone()));
    }

    pub(super) fn window(&mut self, at: &str, op: &Op, window: &Window) {
        let Some((source, out)) = self.streamed(at, op, "a window") else {
            return;
        };
        if window.size == 0 || window.step == Some(0) {
            let why = "a positive size and step".into();
            self.report(at, Problem::Shape("a window".into(), why));
        }
        self.gives(at, out, &array_of(source.r#type.clone()));
    }

    /// An op that gives some of its items as they are, with an optional key body.
    pub(super) fn selects(&mut self, at: &str, op: &Op, what: &str, key: Option<&Body>) {
        let Some((source, out)) = self.streamed(at, op, what) else {
            return;
        };
        if let Some(key) = key {
            self.body(at, op, key, (Arity::Unary, &[&source.r#type]), None);
        }
        self.gives(at, out, &source.r#type);
    }

    pub(super) fn until(&mut self, at: &str, op: &Op, predicate: &Body) {
        let Some((source, out)) = self.streamed(at, op, "an until") else {
            return;
        };
        let item = [&source.r#type];
        self.body(at, op, predicate, (Arity::Unary, &item), Some(&Type::Bool));
        self.gives(at, out, &source.r#type);
    }

    pub(super) fn if_empty(&mut self, at: &str, op: &Op, fallback: &IfEmpty) {
        let Some((source, out)) = self.ends(at, op, "an if_empty", (None, None)) else {
            return;
        };
        self.value_captures(at, op, "an if_empty", &["source"]);
        self.gives(at, out, &source.r#type);
        match (&fallback.value, &fallback.body) {
            (Some(literal), None) => {
                if !crate::fits(&literal.value, &out.r#type, self.defs) {
                    let named = format!("fallback value of output {}", out.name);
                    self.report(at, Problem::Unfit(named));
                }
            }
            (None, Some(body)) => self.body(at, op, body, (Arity::Zero, &[]), None),
            _ => {
                let why = "a value or a body, exactly one".into();
                self.report(at, Problem::Shape("an if_empty".into(), why));
            }
        }
    }

    /// The stream input `source` and the stream output `out` of an op.
    fn streamed<'o>(&mut self, at: &str, op: &'o Op, what: &str) -> Option<(&'o Port, &'o Port)> {
        let stream = Some(Flow::Stream);
        let ends = self.ends(at, op, what, (stream, stream));
        self.value_captures(at, op, what, &["source"]);
        ends
    }
}
