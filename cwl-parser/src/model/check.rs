//! Checks a loaded workflow as cwltool does before it runs: step links, features and cycles.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{HashMap, HashSet};

use crate::error::Error;
use crate::model::process::{Class, Kind, Process};
use crate::model::requirement::{Requirement, RequirementClass};
use crate::model::schema::{Type, short_name};
use crate::model::typecheck::check_types;
use crate::model::workflow::{Step, Workflow};

/// Checks every workflow below a process; `inherited` are the requirements and hints of the
/// enclosing workflows, which count for feature requirements as in cwltool.
pub fn check_workflows(process: &Process, inherited: &[&Requirement]) -> Result<(), Error> {
    let Class::Workflow(workflow) = &process.class else {
        return Ok(());
    };
    let mut scope: Vec<&Requirement> = inherited.to_vec();
    scope.extend(process.requirements.iter().chain(&process.hints));
    if process
        .outputs
        .iter()
        .any(|output| output.output_source.len() > 1)
        && !scope
            .iter()
            .any(|entry| matches!(entry.class, RequirementClass::MultipleInputFeature))
    {
        return Err(Error::new(
            &process.location,
            "multiple output sources need MultipleInputFeatureRequirement",
        ));
    }
    for step in &workflow.steps {
        let mut step_scope = scope.clone();
        step_scope.extend(step.requirements.iter().chain(&step.hints));
        check_step(step, &step_scope)?;
        check_workflows(&step.run.process, &step_scope)?;
    }
    check_cycles(workflow)?;
    check_types(process, workflow)
}

fn check_step(step: &Step, scope: &[&Requirement]) -> Result<(), Error> {
    let tool = &step.run.process;
    let has = |wanted: fn(&RequirementClass) -> bool| {
        scope.iter().any(|requirement| wanted(&requirement.class))
    };
    if step.inputs.iter().any(|input| input.value_from.is_some())
        && !has(|class| matches!(class, RequirementClass::StepInputExpression))
    {
        return Err(Error::new(
            &step.location,
            "valueFrom needs StepInputExpressionRequirement",
        ));
    }
    if step.inputs.iter().any(|input| input.source.len() > 1)
        && !has(|class| matches!(class, RequirementClass::MultipleInputFeature))
    {
        return Err(Error::new(
            &step.location,
            "multiple sources need MultipleInputFeatureRequirement",
        ));
    }
    if tool.kind() == Kind::Workflow
        && !has(|class| matches!(class, RequirementClass::SubworkflowFeature))
    {
        let message = "the step runs a workflow, but SubworkflowFeatureRequirement is missing";
        return Err(Error::new(&step.location, message));
    }
    if !step.scatter.is_empty() {
        if !has(|class| matches!(class, RequirementClass::ScatterFeature)) {
            let message = "the step scatters, but ScatterFeatureRequirement is missing";
            return Err(Error::new(&step.location, message));
        }
        if step.scatter.len() > 1 && step.scatter_method.is_none() {
            let message = "scattering over several inputs needs a `scatterMethod`";
            return Err(Error::new(&step.location, message));
        }
    }
    let outputs: HashSet<&str> = tool
        .outputs
        .iter()
        .map(|output| short_name(&output.id))
        .collect();
    for output in &step.outputs {
        if !outputs.contains(short_name(output)) {
            let message = format!(
                "the step output `{}` is not an output of the process it runs",
                short_name(output)
            );
            return Err(Error::new(&step.location, message));
        }
    }
    let bound: HashSet<&str> = step
        .inputs
        .iter()
        .map(|input| short_name(&input.id))
        .collect();
    let missing: Vec<&str> = tool
        .inputs
        .iter()
        .filter(|input| input.default.is_none() && !optional(&input.value_type))
        .map(|input| short_name(&input.id))
        .filter(|name| !bound.contains(name))
        .collect();
    if !missing.is_empty() {
        let message = format!(
            "the step does not set the required inputs `{}`",
            missing.join("`, `")
        );
        return Err(Error::new(&step.location, message));
    }
    Ok(())
}

/// Whether a type accepts null, so that an unset input is allowed.
fn optional(value_type: &Type) -> bool {
    match value_type {
        Type::Null => true,
        Type::Union(types) => types.contains(&Type::Null),
        _ => false,
    }
}

/// Fails when steps depend on each other in a circle through their input sources.
fn check_cycles(workflow: &Workflow) -> Result<(), Error> {
    let ids: HashSet<&str> = workflow.steps.iter().map(|step| step.id.as_str()).collect();
    let mut edges: HashMap<&str, Vec<&str>> = HashMap::new();
    for step in &workflow.steps {
        let sources = step.inputs.iter().flat_map(|input| &input.source);
        let owners = sources.filter_map(|source| source.rsplit_once('/').map(|(owner, _)| owner));
        edges.insert(
            &step.id,
            owners.filter(|owner| ids.contains(owner)).collect(),
        );
    }
    let mut done = HashSet::new();
    for step in &workflow.steps {
        let mut path = vec![step.id.as_str()];
        if let Some(cycle) = find_cycle(&edges, &mut path, &mut done) {
            let names: Vec<&str> = cycle.iter().map(|id| short_name(id)).collect();
            let message = format!(
                "steps depend on each other in a circle: {}",
                names.join(", ")
            );
            return Err(Error::new(&step.location, message));
        }
    }
    Ok(())
}

/// A circle of steps reachable from the last step of the path, by depth-first search.
fn find_cycle<'s>(
    edges: &HashMap<&'s str, Vec<&'s str>>,
    path: &mut Vec<&'s str>,
    done: &mut HashSet<&'s str>,
) -> Option<Vec<&'s str>> {
    let current = *path.last()?;
    if done.contains(current) {
        return None;
    }
    for next in edges.get(current).into_iter().flatten() {
        if let Some(start) = path.iter().position(|step| step == next) {
            return Some(path[start..].to_vec());
        }
        path.push(next);
        let found = find_cycle(edges, path, done);
        path.pop();
        if found.is_some() {
            return found;
        }
    }
    done.insert(current);
    None
}
