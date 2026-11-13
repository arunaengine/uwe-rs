//! Steps templates (workflow/controller/steps.go): a list of step groups that run one after the
//! other, each a list of steps that run side by side.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use argo_parser::model::{Artifact, TemplateRef, WorkflowStep};
use argo_parser::{Node, Value as Yaml, decode};
use workflow_graph::TaskId;

use crate::cursor::{Cursor, Entries, deferred};
use crate::items::{items, items_unless_skipped, loops};
use crate::node::{ArtifactValue, NodeKind, Phase};
use crate::run::{Args, Call, Context, Expanded, Holder, Result, Run};
use crate::scope::Scope;
use crate::tags::replace_node;
use crate::when::should_execute;

impl Run {
    pub fn execute_steps(&mut self, index: usize) -> Result<()> {
        let Some(processed) = self.nodes[index].processed.clone() else {
            return Ok(());
        };
        if self.fail_fast(index) {
            self.settle_fail_fast(index);
            return Ok(());
        }
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
        let mut entries = match self.nodes[index].entries.take() {
            Some(entries) => entries,
            None => Box::new(self.expand_steps(index, name, group, scope, steps)?),
        };
        let result = self.walk_steps(index, name, scope, context, &mut entries);
        self.nodes[index].entries = Some(entries);
        result.map(|()| index)
    }

    /// Goes on with the entries of a step group that may still change, and ends the group when
    /// every step, and its hooks, ended.
    fn walk_steps(
        &mut self,
        index: usize,
        name: &str,
        scope: &Scope,
        context: &Rc<Context>,
        entries: &mut Entries,
    ) -> Result<()> {
        let call = Call {
            boundary: self.nodes[index].boundary,
            inbound: self.nodes[index].inbound.clone(),
            depth: self.nodes[index].depth + 1,
        };
        let mut open = Vec::new();
        let mut busy = vec![0; entries.made.len()];
        for &position in &entries.open.clone() {
            let step = Rc::clone(&entries.list[position]);
            let child = match entries.nodes[position] {
                Some(child) => {
                    if !self.nodes[child].phase.fulfilled() {
                        self.reenter(child)?;
                    }
                    child
                }
                None => {
                    let child_name = format!("{name}.{}", step.name);
                    let slot = entries.slots[position];
                    let made = if position < entries.eager {
                        self.start_step(index, &step, &child_name, &call, context, None)?
                    } else {
                        let later = entries.made.iter().skip(slot + 1).sum();
                        self.with_globals(&entries.globals, |run| {
                            run.start_step(index, &step, &child_name, &call, context, Some(later))
                        })?
                    };
                    let Some((child, runs)) = made else {
                        return Ok(());
                    };
                    if let Some(count) = entries.made.get_mut(slot) {
                        *count += 1;
                    }
                    entries.nodes[position] = Some(child);
                    entries.runs[position] = runs;
                    child
                }
            };
            let runs = entries.runs[position];
            if runs && (!step.hooks.is_empty() || !step.on_exit.is_empty()) {
                let prefix = format!("steps.{}", base_name(&step.name));
                self.lifecycle_hooks(child, &step.hooks, scope, &prefix, context)?;
                if self.nodes[child].phase.fulfilled() {
                    let hook = exit_hook(&step);
                    self.exit_hook(child, hook.as_ref(), scope, &prefix, context)?;
                }
            }
            // A step that ended with nothing left to start for it needs no more passes.
            let node = &self.nodes[child];
            let hooks_done = step.hooks.is_empty()
                && (step.on_exit.is_empty()
                    || node
                        .exit_hook
                        .is_some_and(|exit| self.nodes[exit].phase.fulfilled()));
            if !node.phase.fulfilled() || (runs && !hooks_done) {
                open.push(position);
            }
            if !node.phase.fulfilled()
                && let Some(count) = busy.get_mut(entries.slots[position])
            {
                *count += 1;
            }
        }
        entries.open = open;
        if !entries.cursors.is_empty() {
            if self.make_entries(entries, &busy) {
                self.changed = true;
            }
            return Ok(());
        }
        // The group ends when every step, and its hooks, ended.
        for &position in &entries.open {
            let Some(child) = entries.nodes[position] else {
                return Ok(());
            };
            let node = &self.nodes[child];
            if !node.phase.fulfilled() {
                return Ok(());
            }
            if let Some(exit) = node.exit_hook
                && !self.nodes[exit].phase.fulfilled()
            {
                return Ok(());
            }
            if node
                .hooks
                .iter()
                .any(|&hook| !self.nodes[hook].phase.fulfilled())
            {
                return Ok(());
            }
        }
        // Entries made late sit after the steps declared after theirs; check in declared order.
        let mut order: Vec<usize> = (0..entries.list.len()).collect();
        order.sort_by_key(|&position| entries.slots[position]);
        // Steps with an exit hook that did not start yet start it in the next pass.
        for &position in &order {
            let step = &entries.list[position];
            if let Some(child) = entries.nodes[position]
                && exit_hook(step).is_some()
                && self.nodes[child].exit_hook.is_none()
                && self.nodes[child].kind != NodeKind::Skipped
            {
                return Ok(());
            }
        }
        for &position in &order {
            let step = &entries.list[position];
            let Some(child) = entries.nodes[position] else {
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
                return Ok(());
            }
        }
        self.set_phase(index, Phase::Succeeded, String::new());
        Ok(())
    }

    /// Makes the node of a step, and says whether the step runs; none when its `when` cannot
    /// be decided, which fails the group.
    fn start_step(
        &mut self,
        group: usize,
        step: &Expanded,
        child_name: &str,
        call: &Call,
        context: &Rc<Context>,
        later: Option<usize>,
    ) -> Result<Option<(usize, bool)>> {
        if let Some(error) = &step.error {
            let child = self.end_node(child_name, Phase::Error, call, error.clone());
            self.place_child(group, child, later);
            return Ok(Some((child, false)));
        }
        match self.decide_when(&step.when, child_name) {
            Err(message) => {
                let child = self.end_node(child_name, Phase::Error, call, message.clone());
                self.place_child(group, child, later);
                self.fail(group, Phase::Error, message);
                return Ok(None);
            }
            Ok(false) => {
                let message = format!("when '{}' evaluated false", step.when);
                let child = self.end_node(child_name, Phase::Skipped, call, message);
                self.skipped_outputs(child, &step.holder, context, true);
                self.place_child(group, child, later);
                return Ok(Some((child, false)));
            }
            Ok(true) => {}
        }
        let child = self.execute_template(child_name, &step.holder, context, &step.args, call)?;
        self.place_child(group, child, later);
        Ok(Some((child, true)))
    }

    /// Adds the node of a step to its group: last, or before the `later` nodes of the steps
    /// declared after it, which a cursor made earlier.
    fn place_child(&mut self, group: usize, child: usize, later: Option<usize>) {
        let Some(later) = later else {
            return self.add_child(group, child);
        };
        let children = &mut self.nodes[group].children;
        children.insert(children.len().saturating_sub(later), child);
    }

    pub fn add_child(&mut self, parent: usize, child: usize) {
        if parent != child && !self.nodes[parent].children.contains(&child) {
            self.nodes[parent].children.push(child);
        }
    }

    /// Decides a `when` condition. A condition that still holds a tag cannot be decided: a
    /// real run fails, as Argo cannot parse it; a simulated run, whose outputs are made up,
    /// skips the step with a warning.
    pub fn decide_when(&mut self, when: &str, name: &str) -> std::result::Result<bool, String> {
        if let Some(tag) = crate::tags::first_tag(when)
            && !self.options.assume_inputs
        {
            return Err(format!(
                "when '{when}' of {}: failed to resolve {{{{{tag}}}}}",
                crate::run::display_name(name)
            ));
        }
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
    ) -> Result<Entries> {
        let variables = self.variables(scope);
        let depth = self.nodes[parent].depth;
        let fail_fast = (self.nodes[parent].processed.as_ref())
            .is_some_and(|processed| processed.template.fail_fast == Some(true));
        let mut entries = Entries::default();
        for (slot, step) in steps.iter().enumerate() {
            let step_name = step
                .get("name")
                .and_then(Node::as_str)
                .unwrap_or("")
                .to_owned();
            self.note_deciding_outputs(step, &format!("step `{step_name}`"), scope);
            let mut node = step.clone();
            if let Err(message) = replace_node(&mut node, &variables, true) {
                entries.push(slot, Expanded::error(step_name, message));
                continue;
            }
            let typed = match decode::<WorkflowStep>(&node) {
                Ok((typed, _)) => typed,
                Err(error) => {
                    entries.push(slot, Expanded::error(step_name, error.to_string()));
                    continue;
                }
            };
            let looping = loops(step);
            let proceed = should_execute(&typed.when).unwrap_or(true);
            let artifacts = if proceed {
                match self.step_artifacts(&typed.arguments.artifacts, scope) {
                    Ok(artifacts) => artifacts,
                    Err(message) => {
                        entries.push(slot, Expanded::error(step_name, message));
                        continue;
                    }
                }
            } else {
                Vec::new()
            };
            if !looping {
                entries.push(slot, expanded_step(step_name, node, &typed, artifacts));
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
                    entries.push(slot, Expanded::error(step_name, message));
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
            let mut cursor = Cursor {
                name: step_name,
                slot,
                node,
                template: typed.template.clone(),
                artifacts,
                variables: variables.clone(),
                items: list,
                strict: !matches!(should_execute(&typed.when), Ok(false)),
                next: 0,
                build: step_entry,
            };
            if deferred(&cursor.items, fail_fast) {
                entries.cursors.push(cursor);
                continue;
            }
            while let Some(entry) = self.next_entry(&mut cursor) {
                let entry =
                    entry.unwrap_or_else(|message| Expanded::error(cursor.name.clone(), message));
                entries.push(cursor.slot, entry);
            }
        }
        entries.eager = entries.list.len();
        if !entries.cursors.is_empty() {
            entries.globals = self.globals.clone();
            entries.made = vec![0; steps.len()];
        }
        Ok(entries)
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

/// The entry of one item of a looping step.
pub(crate) fn step_entry(
    name: String,
    node: Node,
    template: &str,
    artifacts: Vec<ArtifactValue>,
) -> Expanded {
    match decode::<WorkflowStep>(&node) {
        Ok((mut step, _)) => {
            step.template = template.to_owned();
            expanded_step(name, node, &step, artifacts)
        }
        Err(error) => Expanded::error(name, error.to_string()),
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
