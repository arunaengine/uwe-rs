//! Verifier checks of loops and dynamic regions: their body ports, condition, carried
//! values and limit, and the plan input or planner of a region.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeSet;

use super::{Checker, Problem, show};
use crate::document::Language;
use crate::op::Op;
use crate::reserved::{Dynamic, Loop, Planner};
use crate::types::{Type, resolve};

impl<'d> Checker<'d> {
    pub(super) fn looping(&mut self, at: &str, op: &Op, looped: &Loop) {
        let mut wrong = Vec::new();
        if looped.limit == Some(0) {
            wrong.push("a positive limit".to_owned());
        }
        if op
            .inputs
            .iter()
            .chain(&op.outputs)
            .any(|port| port.name == looped.condition)
        {
            let why = format!("no port named {}, its condition", looped.condition);
            wrong.push(why);
        }
        let Some(body) = self.graph(&looped.body) else {
            return self.shapes(at, "a loop", wrong);
        };
        if (body.inputs.iter().chain(&body.outputs)).any(|port| !port.flow.is_value()) {
            wrong.push("a body with value ports only".into());
        }
        match body
            .outputs
            .iter()
            .find(|port| port.name == looped.condition)
        {
            Some(port) if resolve(&port.r#type, self.defs) == Some(&Type::Bool) => {}
            _ => wrong.push(format!("a bool body output {}", looped.condition)),
        }
        let mut targets = BTreeSet::new();
        for carry in &looped.carry {
            let from = body.outputs.iter().find(|port| port.name == carry.output);
            let to = body.inputs.iter().find(|port| port.name == carry.input);
            match (from, to) {
                (Some(from), Some(to)) if !to.takes().accepts(&from.r#type, self.defs) => {
                    let (found, want) = (show(&from.r#type), show(&to.r#type));
                    wrong.push(format!("carry {} to take {found}, not {want}", carry.input));
                }
                (Some(_), Some(_)) => {}
                _ => wrong.push(format!(
                    "body ports {} and {} to carry",
                    carry.output, carry.input
                )),
            }
            if !targets.insert(&carry.input) {
                wrong.push(format!("one carry into {}", carry.input));
            }
        }
        for port in &body.inputs {
            let given = op.inputs.iter().find(|item| item.name == port.name);
            match given {
                Some(item)
                    if !item.flow.is_value() || !port.takes().accepts(&item.r#type, self.defs) =>
                {
                    wrong.push(format!("a value input {} that the body takes", port.name));
                }
                Some(_) => {}
                None if port.default.is_some() || port.r#type.is_optional(self.defs) => {}
                None => wrong.push(format!("an input {} for the first run", port.name)),
            }
        }
        for port in &op.outputs {
            match body.outputs.iter().find(|item| item.name == port.name) {
                Some(item) if port.r#type.accepts(&item.r#type, self.defs) => {}
                _ => wrong.push(format!("a body output for output {}", port.name)),
            }
        }
        self.shapes(at, "a loop", wrong);
    }

    pub(super) fn region(&mut self, at: &str, op: &Op, dynamic: &Dynamic) {
        let mut wrong = Vec::new();
        match (&dynamic.plan, &dynamic.planner) {
            (Some(plan), None) => match op.inputs.iter().find(|port| port.name == *plan) {
                Some(port) if resolve(&port.r#type, self.defs) == Some(&Type::Dynamic) => {}
                Some(_) => wrong.push(format!("a dynamic plan input {plan}")),
                None => self.report(at, Problem::NoPort(format!("input {plan}"))),
            },
            (None, Some(Planner::Snakemake(reference))) => {
                for id in [&reference.source].into_iter().chain(&reference.configs) {
                    if let Some(source) = self.source(at, id)
                        && source.language != Language::Snakemake
                    {
                        let (found, want) = (show(&Language::Snakemake), show(&source.language));
                        self.report(
                            at,
                            Problem::Language {
                                named: id.clone(),
                                found,
                                want,
                            },
                        );
                    }
                }
            }
            _ => wrong.push("a plan input or a planner, exactly one".into()),
        }
        self.shapes(at, "a dynamic region", wrong);
    }

    fn shapes(&mut self, at: &str, what: &str, wrong: Vec<String>) {
        for why in wrong {
            self.report(at, Problem::Shape(what.into(), why));
        }
    }
}
