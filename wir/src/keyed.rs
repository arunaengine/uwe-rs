//! Verifier checks of the keyed stream operators: their ports, key positions, sizes, sort
//! rules and join options.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeSet;

use super::callbacks::Arity;
use super::{Checker, Problem};
use crate::document::Port;
use crate::flow::Flow;
use crate::op::Op;
use crate::reserved::{Combine, Cross, GroupTuple, Join, Sort};
use crate::types::{Type, resolve};

/// What positions an item type has.
enum Width {
    /// A tuple of that many items.
    Fixed(usize),
    /// Positions known only at run time, like a dynamic item or a list.
    Open,
    /// One value, keyed only by position 0.
    Scalar,
}

impl<'d> Checker<'d> {
    pub(super) fn group(&mut self, at: &str, op: &Op, group: &GroupTuple) {
        let stream = Some(Flow::Stream);
        let Some((source, _)) = self.ends(at, op, "a group", (stream, stream)) else {
            return;
        };
        self.value_captures(at, op, "a group", &["source"]);
        self.keys(at, &group.by, &[&source.r#type]);
        if group.size == Some(0) {
            self.report(
                at,
                Problem::Shape("a group".into(), "a positive size".into()),
            );
        }
        if group.size.is_some() && group.size_body.is_some() {
            let why = "size or size_body, not both".into();
            self.report(at, Problem::Shape("a group".into(), why));
        }
        if let Some(body) = &group.size_body {
            self.body(at, op, body, (Arity::Unary, &[]), Some(&Type::Int));
        }
        self.sort(at, op, &group.sort);
    }

    pub(super) fn join(&mut self, at: &str, op: &Op, join: &Join) {
        let Some((left, right)) = self.pair(at, op, "a join") else {
            return;
        };
        self.keys(at, &join.by, &[&left.r#type, &right.r#type]);
        if join.remainder && join.mismatch_error {
            let why = "remainder or mismatch_error, not both".into();
            self.report(at, Problem::Shape("a join".into(), why));
        }
        let fixed = [left, right]
            .iter()
            .all(|port| matches!(self.width(&port.r#type), Width::Fixed(_)));
        if join.remainder && !fixed && join.body.is_none() {
            let why = "a body that pads the remainder of open tuples".into();
            self.report(at, Problem::Shape("a join".into(), why));
        }
        if let Some(body) = &join.body {
            self.body(at, op, body, (Arity::Unary, &[]), None);
        }
    }

    pub(super) fn combine(&mut self, at: &str, op: &Op, combine: &Combine) {
        let Some((left, right)) = self.pair(at, op, "a combine") else {
            return;
        };
        if let Some(by) = &combine.by {
            self.keys(at, by, &[&left.r#type, &right.r#type]);
        }
        if let Some(body) = &combine.body {
            self.body(at, op, body, (Arity::Unary, &[]), None);
        }
    }

    pub(super) fn cross(&mut self, at: &str, op: &Op, cross: &Cross) {
        let Some((left, _)) = self.pair(at, op, "a cross") else {
            return;
        };
        if let Some(key) = &cross.key {
            self.body(at, op, key, (Arity::Unary, &[&left.r#type]), None);
        }
    }

    /// The stream inputs `left` and `right` and the stream output `out` of a keyed op.
    fn pair<'o>(&mut self, at: &str, op: &'o Op, what: &str) -> Option<(&'o Port, &'o Port)> {
        let stream = Some(Flow::Stream);
        let left = self.named_input(at, op, what, "left", stream);
        let right = self.named_input(at, op, what, "right", stream);
        if let Some(out) = self.single_out(at, op, what)
            && out.flow.is_value()
        {
            self.report(at, Problem::Shape(what.into(), "a stream output".into()));
        }
        self.value_captures(at, op, what, &["left", "right"]);
        Some((left?, right?))
    }

    /// Key positions are given, unique, and inside the item types where those are known.
    fn keys(&mut self, at: &str, by: &[u32], items: &[&Type]) {
        if by.is_empty() {
            self.report(at, Problem::Empty("by".into()));
        }
        let mut seen = BTreeSet::new();
        for position in by.iter().filter(|position| !seen.insert(**position)) {
            self.report(at, Problem::Duplicate(format!("key position {position}")));
        }
        for item in items {
            match self.width(item) {
                Width::Fixed(width) => {
                    for position in by.iter().filter(|position| **position as usize >= width) {
                        let why = format!("key position {position} inside a tuple of {width}");
                        self.report(at, Problem::Shape("a keyed op".into(), why));
                    }
                }
                Width::Scalar if by != [0] => {
                    let why = "tuple items for key positions besides 0".into();
                    self.report(at, Problem::Shape("a keyed op".into(), why));
                }
                Width::Scalar | Width::Open => {}
            }
        }
    }

    fn width(&self, r#type: &Type) -> Width {
        match resolve(r#type, self.defs) {
            Some(Type::Tuple(items)) => Width::Fixed(items.len()),
            Some(Type::Optional(inner)) => self.width(inner),
            Some(Type::Dynamic | Type::Array(_) | Type::Union(_)) | None => Width::Open,
            Some(_) => Width::Scalar,
        }
    }

    /// A key body takes one item; a comparison takes `left` and `right` and gives an int.
    pub(super) fn sort(&mut self, at: &str, op: &Op, sort: &Sort) {
        match sort {
            Sort::Key(body) => self.body(at, op, body, (Arity::Unary, &[]), None),
            Sort::Compare(body) => self.body(at, op, body, (Arity::Binary, &[]), Some(&Type::Int)),
            _ => {}
        }
    }
}
