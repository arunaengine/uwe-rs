//! Finds connected group jobs and combines their ordered commands in one container task.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use python_evaluator::Interpreter;
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
) -> Result<HashMap<usize, Rc<Vec<usize>>>, python_evaluator::Error> {
    let mut labels = HashMap::new();
    for job in order {
        let entry = &planner.jobs[*job];
        let rule = &planner.rules[entry.rule];
        if !needed.get(job).copied().unwrap_or(false) || rule.localrule || rule.checkpoint {
            continue;
        }
        if let Some(group) = &rule.group {
            let value = if is_callable(interpreter, group) {
                call_input_function(
                    interpreter,
                    group,
                    &NamedList::wildcards(&entry.wildcards),
                    &[],
                )?
            } else {
                group.clone()
            };
            if let Some(label) = value.as_str() {
                labels.insert(
                    *job,
                    crate::pattern::apply(label, &entry.wildcards)
                        .map_err(crate::workflow::workflow_error)?,
                );
            } else if !value.is_none() {
                return Err(python_evaluator::Error::type_error(
                    "Group id must be a string or None",
                ));
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
                return Err(python_evaluator::Error::unsupported(
                    "Group dependencies leave and re-enter the component",
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

pub fn combine(members: &[Task], id: TaskId) -> Result<Task, String> {
    let mut group = crate::services::combine(members, &[], id)?;
    group.inputs.retain(|input| {
        !members.iter().any(|member| {
            member.outputs.iter().any(|output| {
                output.ends_with('/')
                    && std::path::Path::new(&input.path).starts_with(output.trim_end_matches('/'))
            })
        })
    });
    for task in members {
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
    for task in members {
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
    group.command = vec!["sh".into(), "-c".into(), script.clone()];
    group.script = Some(script);
    Ok(group)
}
