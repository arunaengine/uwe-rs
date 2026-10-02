//! Starts lifecycle hooks and exit handlers from workflow and task state.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::rc::Rc;

use argo_parser::model::LifecycleHook;

use crate::run::{Args, Call, Context, Failure, Holder, Result, Run, display_name};
use crate::scope::Scope;
use crate::tags::Variables;
use crate::time;
use crate::value::Value;

impl Run {
    /// Runs the workflow's own hooks whose expression holds, like `workflow.status ==
    /// "Running"`. The exit hook is the exit handler.
    pub(crate) fn workflow_hooks(&mut self, done: bool) -> Result<()> {
        let hooks: Vec<(String, LifecycleHook)> = self
            .spec
            .hooks
            .iter()
            .filter(|(name, _)| name.as_str() != "exit")
            .map(|(name, hook)| (name.clone(), hook.clone()))
            .collect();
        for (name, hook) in hooks {
            let node_name = format!("{}.hooks.{name}", self.workflow_name);
            if let Some(&index) = self.names.get(&node_name) {
                if !self.nodes[index].phase.fulfilled() {
                    self.reenter(index)?;
                }
                continue;
            }
            let status = match (done, self.root) {
                (true, Some(root)) => self.nodes[root].phase.name(),
                _ => "Running",
            };
            self.globals.insert("workflow.status".into(), status.into());
            let variables = Variables::new(self.globals.clone());
            let holds = if hook.expression.is_empty() {
                false
            } else {
                crate::expr::evaluate_bool(&hook.expression, variables.environment())
                    .map_err(|error| Failure::error(format!("hook {name}: {error}")))?
            };
            if !holds {
                continue;
            }
            let args = self.hook_args(&hook, &Scope::default())?;
            let context = Rc::clone(&self.root_context);
            let call = Call {
                inbound: Vec::new(),
                ..Call::default()
            };
            let index =
                self.execute_template(&node_name, &Holder::of_hook(&hook), &context, &args, &call)?;
            self.hook_nodes.push(index);
        }
        Ok(())
    }

    /// The workflow's exit handler, once the entrypoint is done: `spec.onExit` or the hook
    /// `exit`. It sees `workflow.status` and `workflow.failures`.
    pub(crate) fn exit_handler(&mut self, root: usize) -> Result<()> {
        if let Some(index) = self.exit_node {
            if !self.nodes[index].phase.fulfilled() {
                self.reenter(index)?;
            }
            return Ok(());
        }
        let hook = match self.spec.hooks.get("exit") {
            Some(hook) => hook.clone(),
            None if !self.spec.on_exit.is_empty() => LifecycleHook {
                template: self.spec.on_exit.clone(),
                ..LifecycleHook::default()
            },
            None => return Ok(()),
        };
        let phase = self.nodes[root].phase;
        self.globals
            .insert("workflow.status".into(), phase.name().into());
        let failures = self.failures_json();
        self.globals.insert("workflow.failures".into(), failures);
        let args = self.hook_args(&hook, &Scope::default())?;
        let call = Call {
            inbound: self.outbound(root),
            ..Call::default()
        };
        let context = Rc::clone(&self.root_context);
        let name = format!("{}.onExit", self.workflow_name);
        let index =
            self.execute_template(&name, &Holder::of_hook(&hook), &context, &args, &call)?;
        self.exit_node = Some(index);
        Ok(())
    }

    /// `workflow.failures`: the nodes that failed, as JSON in a Go quoted string, as Argo sets
    /// it so that its substitution keeps the escapes.
    fn failures_json(&self) -> String {
        let failures: Vec<String> = self
            .nodes
            .iter()
            .filter(|node| node.phase.failed_or_error())
            .map(|node| {
                let fields = [
                    ("displayName", display_name(&node.name).to_owned()),
                    ("message", node.message.clone()),
                    ("templateName", node.template.clone()),
                    ("phase", node.phase.name().to_owned()),
                    ("podName", self.pod_name(&node.name, &node.template)),
                    (
                        "finishedAt",
                        time::rfc3339(node.finished.unwrap_or(node.started)),
                    ),
                ];
                let fields: Vec<String> = fields
                    .into_iter()
                    .map(|(key, value)| format!("\"{key}\":{}", Value::String(value).to_json()))
                    .collect();
                format!("{{{}}}", fields.join(","))
            })
            .collect();
        let json = if failures.is_empty() {
            "null".to_owned()
        } else {
            format!("[{}]", failures.join(","))
        };
        crate::value::go_quote(&json)
    }

    /// The arguments of a hook, with the tags that refer to the scope substituted.
    pub(crate) fn hook_args(&mut self, hook: &LifecycleHook, scope: &Scope) -> Result<Args> {
        let variables = self.variables(scope);
        let mut parameters = hook.arguments.parameters.clone();
        for parameter in &mut parameters {
            if let Some(value) = &mut parameter.value {
                value.0 =
                    crate::tags::replace(&value.0, &variables, true).map_err(Failure::error)?;
            }
        }
        let mut artifacts = Vec::new();
        for artifact in &hook.arguments.artifacts {
            match self.argument_artifact(artifact, scope) {
                Ok(Some(value)) => artifacts.push(value),
                Ok(None) => {}
                Err(error) => return Err(Failure::error(error)),
            }
        }
        Ok(Args {
            parameters,
            artifacts,
        })
    }

    /// Runs the exit hook of a node that finished, and says whether the hook is done.
    pub(crate) fn exit_hook(
        &mut self,
        index: usize,
        hook: Option<&LifecycleHook>,
        scope: &Scope,
        prefix: &str,
        context: &Rc<Context>,
    ) -> Result<bool> {
        if let Some(exit) = self.nodes[index].exit_hook {
            if !self.nodes[exit].phase.fulfilled() {
                self.reenter(exit)?;
            }
            return Ok(self.nodes[exit].phase.fulfilled());
        }
        let Some(hook) = hook else {
            return Ok(true);
        };
        if !hook.expression.is_empty() {
            let mut scope = scope.clone();
            self.add_node_scope(&mut scope, prefix, index);
            let variables = self.variables(&scope);
            let holds = crate::expr::evaluate_bool(&hook.expression, variables.environment())
                .map_err(|error| Failure::error(error.message))?;
            if !holds {
                return Ok(true);
            }
        }
        let mut hook_scope = scope.clone();
        self.add_node_scope(&mut hook_scope, prefix, index);
        let args = self.hook_args(hook, &hook_scope)?;
        let name = format!("{}.onExit", self.nodes[index].name);
        let call = Call {
            boundary: self.nodes[index].boundary,
            inbound: self.outbound(index),
            depth: self.nodes[index].depth + 1,
        };
        let exit = self.execute_template(&name, &Holder::of_hook(hook), context, &args, &call)?;
        self.nodes[index].exit_hook = Some(exit);
        Ok(self.nodes[exit].phase.fulfilled())
    }

    /// Runs the lifecycle hooks of a node whose expressions hold, and says whether all hooks
    /// that started are done.
    pub(crate) fn lifecycle_hooks(
        &mut self,
        index: usize,
        hooks: &BTreeMap<String, LifecycleHook>,
        scope: &Scope,
        prefix: &str,
        context: &Rc<Context>,
    ) -> Result<bool> {
        let mut done = true;
        for (hook_name, hook) in hooks {
            if hook_name == "exit" {
                continue;
            }
            let name = format!("{}.hooks.{hook_name}", self.nodes[index].name);
            if let Some(&existing) = self.names.get(&name) {
                if !self.nodes[existing].phase.fulfilled() {
                    self.reenter(existing)?;
                }
                done &= self.nodes[existing].phase.fulfilled();
                continue;
            }
            if hook.expression.is_empty() {
                return Err(Failure::error(format!(
                    "expression required for hook {name}"
                )));
            }
            let mut scope = scope.clone();
            self.add_node_scope(&mut scope, prefix, index);
            let variables = self.variables(&scope);
            let holds = crate::expr::evaluate_bool(&hook.expression, variables.environment())
                .map_err(|error| Failure::error(format!("hook {name}: {error}")))?;
            if !holds {
                continue;
            }
            let args = self.hook_args(hook, &scope)?;
            let call = Call {
                boundary: self.nodes[index].boundary,
                inbound: self.nodes[index].inbound.clone(),
                depth: self.nodes[index].depth + 1,
            };
            let hook_node =
                self.execute_template(&name, &Holder::of_hook(hook), context, &args, &call)?;
            self.nodes[index].hooks.push(hook_node);
            done &= self.nodes[hook_node].phase.fulfilled();
        }
        Ok(done)
    }
}
