//! Finds connected group jobs and combines their ordered commands in one container task.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use python_evaluator::{Interpreter, Key, Value};
use workflow_graph::{Reuse, Task, TaskId};

use crate::functions::{call_input_function, is_callable};
use crate::objects::NamedList;
use crate::plan::Planner;
use crate::shell::quote;

pub fn components(
    interpreter: &mut Interpreter,
    planner: &Planner,
    order: &[usize],
    needed: &HashMap<usize, bool>,
    fixed: &HashMap<usize, TaskId>,
) -> Result<HashMap<usize, Rc<Vec<usize>>>, python_evaluator::Error> {
    let mut labels = HashMap::new();
    for job in order {
        let entry = &planner.jobs[*job];
        let rule = &planner.rules[entry.rule];
        if !needed.get(job).copied().unwrap_or(false)
            || rule.localrule
            || rule.checkpoint
            || entry.incomplete
            || fixed.contains_key(job)
        {
            continue;
        }
        if let Some(group) = &rule.group {
            let callable = is_callable(interpreter, group);
            let value = if callable {
                call_input_function(
                    interpreter,
                    group,
                    &NamedList::wildcards(&entry.wildcards),
                    &[],
                )?
            } else {
                group.clone()
            };
            let value = if !callable && let Some(label) = value.as_str() {
                Value::str(
                    crate::pattern::apply(label, &entry.wildcards)
                        .map_err(crate::workflow::workflow_error)?,
                )
            } else {
                value
            };
            if !value.is_none() {
                labels.insert(*job, Key::new(value)?);
            }
        }
    }
    let mut neighbors: HashMap<usize, Vec<usize>> = HashMap::new();
    for job in order {
        for (_, producer) in &planner.jobs[*job].dependencies {
            if labels.contains_key(job) && labels.get(job) == labels.get(producer) {
                neighbors.entry(*job).or_default().push(*producer);
                neighbors.entry(*producer).or_default().push(*job);
            }
        }
    }
    let mut result = HashMap::new();
    for job in order {
        if !labels.contains_key(job) {
            continue;
        }
        if result.contains_key(job) {
            continue;
        }
        let mut members = HashSet::new();
        let mut pending = vec![*job];
        while let Some(member) = pending.pop() {
            if !members.insert(member) {
                continue;
            }
            pending.extend(neighbors.get(&member).into_iter().flatten().copied());
        }
        let ordered: Vec<usize> = order
            .iter()
            .copied()
            .filter(|job| members.contains(job))
            .collect();
        if ordered
            .iter()
            .any(|job| !crate::services::service_files(&planner.jobs[*job]).is_empty())
        {
            for member in ordered {
                result.insert(member, Rc::new(vec![member]));
            }
            continue;
        }
        let mut external = Vec::new();
        let ordered = Rc::new(ordered);
        for member in ordered.iter() {
            external.extend(
                planner.jobs[*member]
                    .dependencies
                    .iter()
                    .map(|(_, job)| *job)
                    .filter(|job| !members.contains(job)),
            );
        }
        let mut seen = HashSet::new();
        while let Some(job) = external.pop() {
            if members.contains(&job) {
                return Err(crate::workflow::workflow_error(
                    "Error in group: dependencies leave and re-enter the component through jobs outside the group",
                ));
            }
            if seen.insert(job) {
                external.extend(planner.jobs[job].dependencies.iter().map(|(_, job)| *job));
            }
        }
        for member in ordered.iter() {
            result.insert(*member, ordered.clone());
        }
    }
    Ok(result)
}

/// The layer of each job of a group: one more than the latest job of the group it waits for.
pub fn layers(planner: &Planner, jobs: &[usize]) -> Vec<usize> {
    let mut layers: Vec<usize> = Vec::with_capacity(jobs.len());
    for job in jobs {
        let layer = (jobs.iter().zip(&layers))
            .filter(|(other, _)| {
                (planner.jobs[*job].dependencies.iter()).any(|(_, producer)| producer == *other)
            })
            .map(|(_, layer)| layer + 1)
            .max()
            .unwrap_or(0);
        layers.push(layer);
    }
    layers
}

pub fn combine(
    members: &[Task],
    id: TaskId,
    pipes: &[String],
    services: &[String],
) -> Result<Task, String> {
    let mut members = members.to_vec();
    let mut conflicts = HashSet::new();
    let mut staged = HashMap::new();
    for task in &members {
        for input in &task.inputs {
            if input.path.starts_with(crate::embedded::SCRIPTS_FOLDER)
                && let Some(known) = staged.insert(input.path.clone(), input.clone())
                && known != *input
            {
                conflicts.insert(input.path.clone());
            }
        }
    }
    for (number, task) in members.iter_mut().enumerate() {
        for input in &mut task.inputs {
            if conflicts.contains(&input.path) {
                let old = input.path.clone();
                let tail = old.strip_prefix(crate::embedded::SCRIPTS_FOLDER).unwrap();
                input.path = format!("{}/{number}{tail}", crate::embedded::SCRIPTS_FOLDER);
                for argument in &mut task.command {
                    *argument = argument.replace(&old, &input.path);
                }
            }
        }
    }
    let transient: Vec<_> = pipes.iter().chain(services).cloned().collect();
    let mut group = crate::services::combine(&members, &transient, id)?;
    group.inputs.retain(|input| {
        !members.iter().any(|member| {
            member.outputs.iter().any(|output| {
                output.ends_with('/')
                    && std::path::Path::new(&input.path).starts_with(output.trim_end_matches('/'))
            })
        })
    });
    for task in &members {
        for input in &task.inputs {
            if let Some(staged) = group.inputs.iter().find(|staged| staged.path == input.path)
                && staged != input
            {
                return Err(format!(
                    "Group jobs require different contents at staged input {}",
                    input.path
                ));
            }
        }
    }
    let mut script = String::new();
    let mut duration = Some(0_u64);
    group.resources = Default::default();
    for task in &members {
        let exports: String = task
            .environment
            .iter()
            .map(|(name, value)| format!("export {name}={}; ", quote(value)))
            .collect();
        let command = task
            .command
            .iter()
            .map(|word| quote(word))
            .collect::<Vec<_>>()
            .join(" ");
        if !command.is_empty() {
            script.push_str(&format!("( {exports}exec {command} ) || exit $?\n"));
        }
        let own = &task.resources;
        group.resources.cpus = Some(
            group
                .resources
                .cpus
                .unwrap_or(1.0)
                .max(own.cpus.unwrap_or(1.0)),
        );
        group.resources.memory_bytes = group.resources.memory_bytes.max(own.memory_bytes);
        group.resources.disk_bytes = group.resources.disk_bytes.max(own.disk_bytes);
        group.resources.gpus = group.resources.gpus.max(own.gpus);
        duration = duration
            .zip(own.time_seconds)
            .map(|(sum, time)| sum.saturating_add(time));
        if task.reuse != Reuse::Allowed {
            group.reuse = task.reuse;
        }
    }
    group.resources.time_seconds = duration;
    // Snakemake runs the jobs of a group at once as far as their files allow, and jobs of a
    // group may share files they do not declare (test_group_parallel).
    let parallel = members.len() > 1;
    if !pipes.is_empty() {
        group.resources = crate::services::resources(&members);
    } else if parallel {
        layered(&members, &mut group.resources);
    }
    if parallel {
        script = crate::pipes::script(&members, pipes, services);
    }
    group.command = vec![
        if parallel { "bash" } else { "sh" }.into(),
        "-c".into(),
        script.clone(),
    ];
    group.script = Some(script);
    Ok(group)
}

/// The cores and memory of a group whose jobs run at once when their inputs are ready: the
/// most that the jobs of one layer of the group need together.
fn layered(members: &[Task], resources: &mut workflow_graph::Resources) {
    let mut layers: Vec<usize> = Vec::with_capacity(members.len());
    for task in members {
        let layer = (members.iter().zip(&layers))
            .filter(|(producer, _)| crate::services::depends_on(task, producer))
            .map(|(_, layer)| layer + 1)
            .max()
            .unwrap_or(0);
        layers.push(layer);
    }
    let mut cpus: HashMap<usize, f64> = HashMap::new();
    let mut memory: HashMap<usize, u64> = HashMap::new();
    for (task, layer) in members.iter().zip(&layers) {
        *cpus.entry(*layer).or_default() += task.resources.cpus.unwrap_or(1.0);
        if let Some(bytes) = task.resources.memory_bytes {
            *memory.entry(*layer).or_default() += bytes;
        }
    }
    resources.cpus = cpus.into_values().reduce(f64::max);
    resources.memory_bytes = memory.into_values().max();
}
