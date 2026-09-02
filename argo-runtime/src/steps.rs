//! Steps templates (workflow/controller/steps.go): a list of step groups that run one after the
//! other, each a list of steps that run side by side.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use argo_parser::model::{Artifact, TemplateRef, WorkflowStep};
use argo_parser::{Node, Value as Yaml, decode};
use workflow_graph::TaskId;

use crate::items::{item_variables, items, items_unless_skipped, loops, substitute_item};
use crate::node::{NodeKind, Phase};
use crate::run::{Args, Call, Context, Expanded, Holder, Result, Run};
use crate::scope::Scope;
use crate::tags::replace_node;
use crate::when::should_execute;

impl Run {
    pub fn execute_steps(&mut self, index: usize) -> Result<()> {
        let Some(processed) = self.nodes[index].processed.clone() else {
            return Ok(());
        };
        let groups: Vec<Vec<Node>> = match processed.node.get("steps").map(|node| &node.value) {
            Some(Yaml::Sequence(groups)) => groups
                .iter()
                .map(|group| match &group.value {
                    Yaml::Sequence(steps) => steps.clone(),
                    _ => Vec::new(),
                })
                .collect(),
            _ => Vec::new(),
        };
        let mut scope = Scope::of_template(&processed);
        let mut inbound = self.nodes[index].inbound.clone();
        let name = self.nodes[index].name.clone();
        for (position, group) in groups.iter().enumerate() {
            let group_name = format!("{name}[{position}]");
            let group_index = self.execute_step_group(
                index,
                &group_name,
                group,
                &scope,
                &inbound,
                &processed.context,
            )?;
            if !self.nodes[group_index].phase.fulfilled() {
                return Ok(());
            }
            if self.nodes[group_index].phase.failed_or_error() {
                let message = self.nodes[group_index].message.clone();
                self.set_phase(index, Phase::Failed, message);
                return Ok(());
            }
            for step in group {
                let step_name = step.get("name").and_then(Node::as_str).unwrap_or("");
                let child_name = format!("{group_name}.{step_name}");
                let prefix = format!("steps.{step_name}");
                match self.names.get(&child_name) {
                    Some(&child) => self.add_node_scope(&mut scope, &prefix, child),
                    None => {
                        let loop_prefix = format!("{child_name}(");
                        let children: Vec<usize> = self.nodes[group_index]
                            .children
                            .iter()
                            .copied()
                            .filter(|&child| {
                                self.nodes[child].name.starts_with(&loop_prefix)
                                    && self.nodes[child].kind != NodeKind::Skipped
                            })
                            .collect();
                        self.add_aggregate_scope(&mut scope, &prefix, &children);
                    }
                }
            }
            inbound = self.outbound(group_index);
        }
        match self.outputs_from_scope(&processed, &scope) {
            Ok(outputs) => {
                self.nodes[index].outputs = outputs;
                self.set_phase(index, Phase::Succeeded, String::new());
            }
            Err(message) => self.fail(index, Phase::Error, message),
        }
        Ok(())
    }

    fn execute_step_group(
        &mut self,
        steps: usize,
        name: &str,
        group: &[Node],
        scope: &Scope,
        inbound: &[TaskId],
        context: &Rc<Context>,
    ) -> Result<usize> {
        let depth = self.nodes[steps].depth;
        let index = match self.names.get(name) {
            Some(&index) if self.nodes[index].phase.fulfilled() => return Ok(index),
            Some(&index) => index,
            None => {
                let call = Call {
                    boundary: Some(steps),
                    inbound: inbound.to_vec(),
                    depth,
                };
                let index = self.new_node(name, NodeKind::StepGroup, "", &call);
                self.nodes[steps].children.push(index);
                index
            }
        };
        let expanded = match self.nodes[index].expanded.clone() {
            Some(expanded) => expanded,
            None => {
                let expanded = Rc::new(self.expand_steps(index, name, group, scope, steps)?);
                self.nodes[index].expanded = Some(Rc::clone(&expanded));
                expanded
            }
        };
        let call = Call {
            boundary: Some(steps),
            inbound: self.nodes[index].inbound.clone(),
            depth: depth + 1,
        };
        for step in expanded.iter() {
            let child_name = format!("{name}.{}", step.name);
            if let Some(error) = &step.error {
                let child = self.end_node(&child_name, Phase::Error, &call, error.clone());
                self.add_child(index, child);
                continue;
            }
            match self.decide_when(&step.when, &child_name) {
                Err(message) => {
                    let child = self.end_node(&child_name, Phase::Error, &call, message.clone());
                    self.add_child(index, child);
                    self.fail(index, Phase::Error, message);
                    return Ok(index);
                }
                Ok(false) => {
                    let message = format!("when '{}' evaluated false", step.when);
                    let child = self.end_node(&child_name, Phase::Skipped, &call, message);
                    self.skipped_outputs(child, &step.holder, context, true);
                    self.add_child(index, child);
                    continue;
                }
                Ok(true) => {}
            }
            let child =
                self.execute_template(&child_name, &step.holder, context, &step.args, &call)?;
            self.add_child(index, child);
            let prefix = format!("steps.{}", base_name(&step.name));
            self.lifecycle_hooks(child, &step.hooks, scope, &prefix, context)?;
            if self.nodes[child].phase.fulfilled() {
                let hook = exit_hook(step);
                self.exit_hook(child, hook.as_ref(), scope, &prefix, context)?;
            }
        }
        // The group ends when every step, and its hooks, ended.
        for &child in &self.nodes[index].children {
            let node = &self.nodes[child];
            if !node.phase.fulfilled() {
                return Ok(index);
            }
            if let Some(exit) = node.exit_hook
                && !self.nodes[exit].phase.fulfilled()
            {
                return Ok(index);
            }
            if node
                .hooks
                .iter()
                .any(|&hook| !self.nodes[hook].phase.fulfilled())
            {
                return Ok(index);
            }
        }
        // Steps with an exit hook that did not start yet start it in the next pass.
        for step in expanded.iter() {
            let child_name = format!("{name}.{}", step.name);
            if let Some(&child) = self.names.get(&child_name)
                && exit_hook(step).is_some()
                && self.nodes[child].exit_hook.is_none()
                && self.nodes[child].kind != NodeKind::Skipped
            {
                return Ok(index);
            }
        }
        for step in expanded.iter() {
            let child_name = format!("{name}.{}", step.name);
            let Some(&child) = self.names.get(&child_name) else {
                continue;
            };
            let phase = self.nodes[child].phase;
            let continues = match phase {
                Phase::Failed => step.continue_on.1,
                Phase::Error => step.continue_on.0,
                _ => true,
            };
            if !continues {
                let message = format!("child '{}' failed", self.nodes[child].id);
                self.set_phase(index, Phase::Failed, message);
                return Ok(index);
            }
        }
        self.set_phase(index, Phase::Succeeded, String::new());
        Ok(index)
    }

    pub fn add_child(&mut self, parent: usize, child: usize) {
        if parent != child && !self.nodes[parent].children.contains(&child) {
            self.nodes[parent].children.push(child);
        }
    }

    /// Decides a `when` condition. A condition that still holds a tag, as a run with empty
    /// outputs or placeholder parameters leaves some, cannot be decided: the step is skipped,
    /// with a warning.
    pub fn decide_when(&mut self, when: &str, name: &str) -> std::result::Result<bool, String> {
        if crate::tags::has_tag(when) {
            self.warn(format!(
                "cannot decide when '{when}' of {}: a tag is unresolved; it is skipped",
                crate::run::display_name(name)
            ));
            return Ok(false);
        }
        should_execute(when)
    }

    /// Substitutes the steps of a group with the scope, resolves their artifacts and expands
    /// their loops.
    fn expand_steps(
        &mut self,
        group: usize,
        name: &str,
        steps: &[Node],
        scope: &Scope,
        parent: usize,
    ) -> Result<Vec<Expanded>> {
        let variables = self.variables(scope);
        let depth = self.nodes[parent].depth;
        let mut expanded = Vec::new();
        for step in steps {
            let step_name = step
                .get("name")
                .and_then(Node::as_str)
                .unwrap_or("")
                .to_owned();
            self.note_deciding_outputs(step, &format!("step `{step_name}`"), scope);
            let mut node = step.clone();
            if let Err(message) = replace_node(&mut node, &variables, true) {
                expanded.push(Expanded::error(step_name, message));
                continue;
            }
            let typed = match decode::<WorkflowStep>(&node) {
                Ok((typed, _)) => typed,
                Err(error) => {
                    expanded.push(Expanded::error(step_name, error.to_string()));
                    continue;
                }
            };
            let looping = loops(step);
            let proceed = should_execute(&typed.when).unwrap_or(true);
            let artifacts = if proceed {
                match self.step_artifacts(&typed.arguments.artifacts, scope) {
                    Ok(artifacts) => artifacts,
                    Err(message) => {
                        expanded.push(Expanded::error(step_name, message));
                        continue;
                    }
                }
            } else {
                Vec::new()
            };
            if !looping {
                expanded.push(expanded_step(step_name, node, &typed, artifacts));
                continue;
            }
            let mut warnings = Vec::new();
            let list = items(
                &node,
                typed.with_sequence.as_ref(),
                self.options.assume_inputs,
                &mut |warning| warnings.push(warning),
            );
            for warning in warnings {
                self.warn(warning);
            }
            let list = match items_unless_skipped(&typed.when, list) {
                Ok(list) => list,
                Err(message) => {
                    expanded.push(Expanded::error(step_name, message));
                    continue;
                }
            };
            if list.is_empty() {
                let call = Call {
                    boundary: Some(parent),
                    inbound: self.nodes[group].inbound.clone(),
                    depth: depth + 1,
                };
                let step_holder = holder(
                    &typed.template,
                    typed.template_ref.as_ref(),
                    node.get("inline"),
                );
                let context = self.nodes[parent]
                    .processed
                    .as_ref()
                    .map(|processed| Rc::clone(&processed.context))
                    .unwrap_or_else(|| Rc::clone(&self.root_context));
                let child = self.empty_loop(
                    &format!("{name}.{step_name}"),
                    &step_holder,
                    &context,
                    &call,
                );
                self.add_child(group, child);
                continue;
            }
            let strict = !matches!(should_execute(&typed.when), Ok(false));
            for (position, item) in list.iter().enumerate() {
                let (item_variables, item_name) = match item_variables(&step_name, position, item) {
                    Ok(found) => found,
                    Err(message) => {
                        expanded.push(Expanded::error(step_name.clone(), message));
                        break;
                    }
                };
                let strict = if self.options.assume_inputs && crate::items::unknown_item(item) {
                    self.warn(format!(
                        "an item of the loop {item_name} is empty; tags that read its fields stay"
                    ));
                    false
                } else {
                    strict
                };
                let item_variables = variables.clone().with_values(item_variables);
                match substitute_item(&node, &item_variables, strict) {
                    Ok(item_node) => match decode::<WorkflowStep>(&item_node) {
                        Ok((item_step, _)) => {
                            let mut item_step = item_step;
                            item_step.template = typed.template.clone();
                            expanded.push(expanded_step(
                                item_name,
                                item_node,
                                &item_step,
                                artifacts.clone(),
                            ));
                        }
                        Err(error) => expanded.push(Expanded::error(item_name, error.to_string())),
                    },
                    Err(message) => expanded.push(Expanded::error(item_name, message)),
                }
            }
        }
        Ok(expanded)
    }

    /// The artifact arguments of a step or task.
    pub fn step_artifacts(
        &mut self,
        artifacts: &[Artifact],
        scope: &Scope,
    ) -> std::result::Result<Vec<crate::node::ArtifactValue>, String> {
        let mut values = Vec::new();
        for artifact in artifacts {
            if let Some(value) = self.argument_artifact(artifact, scope)? {
                values.push(value);
            }
        }
        Ok(values)
    }
}

impl Expanded {
    pub fn error(name: String, message: String) -> Self {
        Expanded {
            name,
            holder: Holder::default(),
            when: String::new(),
            args: Args::default(),
            continue_on: (false, false),
            hooks: Default::default(),
            on_exit: String::new(),
            error: Some(message),
        }
    }
}

fn expanded_step(
    name: String,
    node: Node,
    step: &WorkflowStep,
    artifacts: Vec<crate::node::ArtifactValue>,
) -> Expanded {
    let continue_on = step.continue_on.map_or((false, false), |continue_on| {
        (continue_on.error, continue_on.failed)
    });
    Expanded {
        name,
        holder: holder(
            &step.template,
            step.template_ref.as_ref(),
            node.get("inline"),
        ),
        when: step.when.clone(),
        args: Args {
            parameters: step.arguments.parameters.clone(),
            artifacts,
        },
        continue_on,
        hooks: step.hooks.clone(),
        on_exit: step.on_exit.clone(),
        error: None,
    }
}

pub(crate) fn holder(
    template: &str,
    template_ref: Option<&TemplateRef>,
    inline: Option<&Node>,
) -> Holder {
    Holder {
        template: template.to_owned(),
        template_ref: template_ref.cloned(),
        inline: inline.filter(|node| node.value != Yaml::Null).cloned(),
    }
}

/// The exit hook of a step or task: its `onExit` template or its hook `exit`.
pub(crate) fn exit_hook(expanded: &Expanded) -> Option<argo_parser::model::LifecycleHook> {
    if let Some(hook) = expanded.hooks.get("exit") {
        return Some(hook.clone());
    }
    (!expanded.on_exit.is_empty()).then(|| argo_parser::model::LifecycleHook {
        template: expanded.on_exit.clone(),
        ..Default::default()
    })
}

/// A step name without its loop suffix, `a` of `a(0:x)`.
pub(crate) fn base_name(name: &str) -> &str {
    name.split('(').next().unwrap_or(name)
}
