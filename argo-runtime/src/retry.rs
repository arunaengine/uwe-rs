//! Decides retry policy, backoff and limits for Argo template calls.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::rc::Rc;

use argo_parser::decode;
use argo_parser::model::Template;

use crate::node::{NodeKind, Phase};
use crate::run::{Call, Failure, Processed, Result, Run};
use crate::tags::{Variables, replace_node};

pub(crate) fn check_strategy(strategy: &argo_parser::model::RetryStrategy) -> Result<()> {
    if !matches!(
        strategy.retry_policy.as_str(),
        "" | "Always" | "OnError" | "OnFailure" | "OnTransientError"
    ) {
        return Err(Failure::error(format!(
            "{} is not a valid RetryPolicy",
            strategy.retry_policy
        )));
    }
    if let Some(limit) = &strategy.limit
        && !crate::tags::has_tag(&limit.0)
        && limit.0.trim().parse::<usize>().is_err()
    {
        return Err(Failure::error(
            "retryStrategy.limit must be a nonnegative integer",
        ));
    }
    if let Some(backoff) = &strategy.backoff {
        for key in ["duration", "cap", "maxDuration"] {
            let text = backoff
                .get(key)
                .map(crate::run::scalar_text)
                .unwrap_or_default();
            if crate::tags::has_tag(&text) {
                continue;
            }
            if key == "duration" || !text.is_empty() {
                parse_duration(&text).map_err(|failure| {
                    Failure::error(format!("retryStrategy.backoff.{key}: {}", failure.message))
                })?;
            }
        }
        if let Some(factor) = backoff.get("factor") {
            let text = crate::run::scalar_text(factor);
            if !crate::tags::has_tag(&text) && text.parse::<i32>().is_err() {
                return Err(Failure::error(
                    "retryStrategy.backoff.factor must be an integer",
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "retry_tests.rs"]
mod tests;

fn parse_duration(text: &str) -> Result<std::time::Duration> {
    crate::resources::duration(text)
        .ok_or_else(|| Failure::error(format!("invalid duration {text}")))
}

fn backoff_delay(backoff: &argo_parser::Node, attempts: usize) -> Result<std::time::Duration> {
    let text = |key| {
        backoff
            .get(key)
            .map(crate::run::scalar_text)
            .unwrap_or_default()
    };
    let base = parse_duration(&text("duration"))?;
    let factor = match text("factor").as_str() {
        "" => 1,
        value => value
            .parse::<i32>()
            .map_err(|_| Failure::error("retryStrategy.backoff.factor must be an integer"))?
            .max(1),
    };
    let multiplier = (factor as f64).powf(attempts.saturating_sub(1) as f64);
    let seconds = (base.as_secs_f64() * multiplier).min(i64::MAX as f64 / 1e9);
    let delay = std::time::Duration::try_from_secs_f64(seconds)
        .map_err(|_| Failure::error("retryStrategy backoff overflow"))?;
    match text("cap").as_str() {
        "" => Ok(delay),
        cap => Ok(delay.min(parse_duration(cap)?)),
    }
}

impl Run {
    /// Makes an attempt of a retry node, or ends the node by its last attempt.
    pub(crate) fn continue_retry(&mut self, index: usize) -> Result<()> {
        let Some(call) = self.nodes[index].retry.clone() else {
            return Ok(());
        };
        check_strategy(&call.strategy)?;
        if let Some(message) = self.expired.get(&index).cloned() {
            if self.nodes[index]
                .children
                .iter()
                .all(|&child| self.nodes[child].phase.fulfilled())
            {
                self.expired.remove(&index);
                self.first_failure = Some(message.clone());
                self.fail(index, Phase::Failed, message);
            }
            return Ok(());
        }
        self.nodes[index].processed = Some(Rc::clone(&call.processed));
        self.arm_template(index)?;
        if !self.lock_template(index, &call.processed)? {
            return Ok(());
        }
        let limit = call
            .strategy
            .limit
            .as_ref()
            .map(|limit| {
                let variables = Variables::new(self.globals.clone());
                let text =
                    crate::tags::replace(&limit.0, &variables, true).map_err(Failure::error)?;
                text.trim().parse::<usize>().map_err(|_| {
                    Failure::error("retryStrategy.limit must resolve to a nonnegative integer")
                })
            })
            .transpose()?;
        if let Some(&last) = self.nodes[index].children.last() {
            if !self.nodes[last].phase.fulfilled() {
                return self.reenter(last);
            }
            let phase = self.nodes[last].phase;
            let attempts = self.nodes[index].children.len();
            let policy = match call.strategy.retry_policy.as_str() {
                "" if !call.strategy.expression.is_empty() => "Always",
                "" => "OnFailure",
                policy => policy,
            };
            let retry = match phase {
                Phase::Failed => matches!(policy, "OnFailure" | "Always"),
                Phase::Error => {
                    matches!(policy, "OnError" | "Always")
                        || (policy == "OnTransientError"
                            && self.nodes[last].outcomes.iter().flatten().any(|outcome| {
                                outcome.error == Some(workflow_graph::TaskError::Transient)
                            }))
                }
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
                        .map_err(|error| {
                            Failure::error(format!("retryStrategy.expression: {error}"))
                        })?;
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
            if let Some(backoff) = &call.strategy.backoff {
                let mut backoff = backoff.clone();
                replace_node(&mut backoff, &Variables::new(self.globals.clone()), false)
                    .map_err(Failure::error)?;
                let delay = backoff_delay(&backoff, attempts)?;
                let ended = self.nodes[last].finished_at.unwrap_or(self.clock);
                let waiting = ended
                    .checked_add(delay)
                    .ok_or_else(|| Failure::error("retryStrategy backoff overflow"))?;
                let maximum = backoff
                    .get("maxDuration")
                    .map(crate::run::scalar_text)
                    .filter(|text| !text.is_empty())
                    .map(|text| parse_duration(&text))
                    .transpose()?;
                if let Some(maximum) = maximum {
                    let first = self.nodes[index].children[0];
                    let deadline = self.nodes[first]
                        .started_at
                        .checked_add(maximum)
                        .ok_or_else(|| Failure::error("retryStrategy maxDuration overflow"))?;
                    let message = if self.clock >= deadline {
                        Some("Max duration limit exceeded")
                    } else if waiting > deadline {
                        Some("Backoff would exceed max duration limit")
                    } else {
                        None
                    };
                    if let Some(message) = message {
                        self.first_failure = Some(message.into());
                        self.set_phase(index, phase, message.into());
                        return Ok(());
                    }
                }
                if self.clock < waiting {
                    self.waits.insert(index, waiting);
                    return Ok(());
                }
                self.waits.remove(&index);
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
        crate::run::remove_key(&mut node, "synchronization");
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
        if attempt > 0
            && let Some(maximum) = call
                .strategy
                .backoff
                .as_ref()
                .and_then(|backoff| backoff.get("maxDuration"))
        {
            let text = crate::tags::replace(
                &crate::run::scalar_text(maximum),
                &Variables::new(self.globals.clone()),
                false,
            )
            .map_err(Failure::error)?;
            if !text.is_empty() {
                let first = self.nodes[index].children[0];
                self.nodes[child].retry_limit = Some(
                    self.nodes[first]
                        .started_at
                        .checked_add(parse_duration(&text)?)
                        .ok_or_else(|| Failure::error("retryStrategy maxDuration overflow"))?,
                );
            }
        }
        self.nodes[index].children.push(child);
        self.start_node(child, processed)?;
        if self.nodes[child].phase.fulfilled() {
            self.changed = true;
        }
        Ok(())
    }
}
