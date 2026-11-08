//! Verifier checks of the ops that reduce a stream to a value: collect, to_list, reduce,
//! first and last.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::callbacks::Arity;
use super::kinds::array_of;
use super::{Checker, Problem};
use crate::closure::Body;
use crate::document::Port;
use crate::flow::Flow;
use crate::op::Op;
use crate::reserved::{Collect, EmptyRule, FoldNull, Reduce, Reduction};
use crate::types::{Type, resolve};

impl<'d> Checker<'d> {
    pub(super) fn collect(&mut self, at: &str, op: &Op, collect: &Collect) {
        let Some((source, out)) = self.reduced(at, op, "a collect") else {
            return;
        };
        let mut item = source.r#type.clone();
        if let Some(body) = &collect.body {
            self.body(at, op, body, (Arity::Unary, &[&source.r#type]), None);
            item = self.gives_type(body).cloned().unwrap_or(Type::Dynamic);
        }
        if collect.flat
            && let Some(Type::Array(array)) = resolve(&item, self.defs)
        {
            item = (*array.items).clone();
        }
        self.sort(at, op, &collect.sort);
        self.gives(at, out, &array_of(item));
    }

    pub(super) fn listed(&mut self, at: &str, op: &Op) {
        if let Some((source, out)) = self.reduced(at, op, "a to_list") {
            self.gives(at, out, &array_of(source.r#type.clone()));
        }
    }

    pub(super) fn reduce(&mut self, at: &str, op: &Op, reduce: &Reduce) {
        let Some((source, out)) = self.reduced(at, op, "a reduce") else {
            return;
        };
        let rule = reduce.rule;
        let item = [&source.r#type];
        let mut wrong = Vec::new();
        match (rule, &reduce.body) {
            (Reduction::Fold, None) => wrong.push("a fold needs a body"),
            (Reduction::Fold, Some(body)) => self.body(at, op, body, (Arity::Fold, &[]), None),
            (Reduction::Count, Some(body)) => {
                self.body(at, op, body, (Arity::Unary, &item), Some(&Type::Bool));
            }
            (_, Some(body)) => self.body(at, op, body, (Arity::Unary, &item), None),
            (_, None) => {}
        }
        match (rule, &reduce.compare) {
            (Reduction::Min | Reduction::Max, Some(_)) if reduce.body.is_some() => {
                wrong.push("a key body or a comparison, not both");
            }
            (Reduction::Min | Reduction::Max, Some(compare)) => {
                self.body(at, op, compare, (Arity::Binary, &item), Some(&Type::Int));
            }
            (_, Some(_)) => wrong.push("a comparison only for min and max"),
            (_, None) => {}
        }
        if reduce.seed.is_some() && rule != Reduction::Fold {
            wrong.push("a seed only for a fold");
        }
        if reduce.nulls != FoldNull::Keep && rule != Reduction::Fold {
            wrong.push("a null rule only for a fold");
        }
        for why in wrong {
            self.report(at, Problem::Shape("a reduce".into(), why.into()));
        }
        if rule == Reduction::Count {
            self.gives(at, out, &Type::Int);
        }
        self.empty_value(at, out, &reduce.empty);
    }

    /// A first or a last: a predicate on the item, and an empty value that fits the output.
    pub(super) fn pick(
        &mut self,
        at: &str,
        op: &Op,
        what: &str,
        predicate: &Option<Body>,
        empty: &EmptyRule,
    ) {
        let Some((source, out)) = self.reduced(at, op, what) else {
            return;
        };
        if let Some(body) = predicate {
            let item = [&source.r#type];
            self.body(at, op, body, (Arity::Unary, &item), Some(&Type::Bool));
        }
        self.gives(at, out, &source.r#type);
        self.empty_value(at, out, empty);
    }

    /// The stream input `source` and the value output `out` of a reducing op.
    fn reduced<'o>(&mut self, at: &str, op: &'o Op, what: &str) -> Option<(&'o Port, &'o Port)> {
        let flows = (Some(Flow::Stream), Some(Flow::Value));
        let ends = self.ends(at, op, what, flows);
        self.value_captures(at, op, what, &["source"]);
        ends
    }

    fn empty_value(&mut self, at: &str, out: &Port, empty: &EmptyRule) {
        if let EmptyRule::Value(value) = empty
            && !crate::fits(value, &out.r#type, self.defs)
        {
            self.report(
                at,
                Problem::Unfit(format!("empty value of output {}", out.name)),
            );
        }
    }
}
