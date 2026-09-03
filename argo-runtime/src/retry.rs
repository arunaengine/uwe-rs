//! Retries, after `processNodeRetries` of Argo's operator (workflow/controller/operator.go): a
//! retry node makes attempts of its call until one succeeds, the policy or the expression stops
//! it, or the limit is reached.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::rc::Rc;

use argo_parser::decode;
use argo_parser::model::Template;

use crate::node::{NodeKind, Phase};
use crate::run::{Call, Failure, Processed, Result, Run};
use crate::tags::{Variables, replace_node};

impl Run {
    /// Makes an attempt of a retry node, or ends the node by its last attempt.
    pub(crate) fn continue_retry(&mut self, index: usize) -> Result<()> {
        let Some(call) = self.nodes[index].retry.clone() else {
            return Ok(());
        };
        if let Some(&last) = self.nodes[index].children.last() {
            if !self.nodes[last].phase.fulfilled() {
                return self.reenter(last);
            }
            let phase = self.nodes[last].phase;
            let attempts = self.nodes[index].children.len();
            let limit = call
                .strategy
                .limit
                .as_ref()
                .and_then(|limit| limit.0.trim().parse::<usize>().ok());
            let policy = match call.strategy.retry_policy.as_str() {
                "" if !call.strategy.expression.is_empty() => "Always",
                "" => "OnFailure",
                policy => policy,
            };
            let retry = match phase {
                Phase::Failed => matches!(policy, "OnFailure" | "Always"),
                Phase::Error => matches!(policy, "OnError" | "Always" | "OnTransientError"),
                _ => false,
            };
            let mut retry = retry && limit.is_none_or(|limit| attempts <= limit);
            if retry && !call.strategy.expression.is_empty() {
                let mut values = self.globals.clone();
                let last_node = &self.nodes[last];
                values.insert("lastRetry.status".into(), phase.name().into());
                values.insert(
                    "lastRetry.exitCode".into(),
                    last_node
                        .outputs
                        .exit_code
                        .clone()
                        .unwrap_or_else(|| "-1".into()),
                );
                values.insert("lastRetry.message".into(), last_node.message.clone());
                let duration = (last_node
                    .finished
                    .unwrap_or(last_node.started)
                    .as_nanosecond()
                    - last_node.started.as_nanosecond())
                    / 1_000_000_000;
                values.insert("lastRetry.duration".into(), duration.max(0).to_string());
                let variables = Variables::new(values);
                retry =
                    crate::expr::evaluate_bool(&call.strategy.expression, variables.environment())
                        .unwrap_or(false);
            }
            if !retry {
                let outputs = self.nodes[last].outputs.clone();
                let message = self.nodes[last].message.clone();
                self.nodes[index].outputs = outputs;
                let phase = if phase == Phase::Skipped {
                    Phase::Succeeded
                } else {
                    phase
                };
                self.set_phase(index, phase, message);
                return Ok(());
            }
        }
        // A new attempt, which sees its number as `retries`.
        let attempt = self.nodes[index].children.len();
        let name = format!("{}({attempt})", self.nodes[index].name);
        let mut locals = BTreeMap::new();
        locals.insert("retries".to_owned(), attempt.to_string());
        let template_name = self.nodes[index].template.clone();
        locals.insert("pod.name".to_owned(), self.pod_name(&name, &template_name));
        let mut node = call.processed.node.clone();
        let variables = Variables::new(locals);
        replace_node(&mut node, &variables, true).map_err(Failure::error)?;
        let processed = match decode::<Template>(&node) {
            Ok((template, _)) => Rc::new(Processed {
                node,
                template,
                context: Rc::clone(&call.processed.context),
                artifacts: call.processed.artifacts.clone(),
            }),
            Err(error) => {
                let message = format!("{name}: {error}");
                self.fail(index, Phase::Error, message);
                return Ok(());
            }
        };
        let inbound = match self.nodes[index].children.last() {
            Some(&previous) => self.outbound(previous),
            None => self.nodes[index].inbound.clone(),
        };
        let call_info = Call {
            boundary: self.nodes[index].boundary,
            inbound,
            depth: self.nodes[index].depth,
        };
        let child = self.new_node(&name, NodeKind::Pod, &template_name, &call_info);
        self.nodes[index].children.push(child);
        self.start_node(child, processed)?;
        if self.nodes[child].phase.fulfilled() {
            self.changed = true;
        }
        Ok(())
    }
}
