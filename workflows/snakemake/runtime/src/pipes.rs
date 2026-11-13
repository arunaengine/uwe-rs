//! Connects FIFO jobs and runs their commands together with group failure handling.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use workflow_graph::Task;

use crate::plan::Planner;
use crate::shell::quote;

pub fn files(planner: &Planner, jobs: &[usize]) -> Vec<String> {
    jobs.iter()
        .flat_map(|job| &planner.jobs[*job].output.values)
        .filter(|value| value.tag("pipe").is_some())
        .filter_map(|value| value.as_str().map(str::to_owned))
        .collect()
}

pub fn components(
    planner: &Planner,
    order: &[usize],
    needed: &mut HashMap<usize, bool>,
    groups: &mut HashMap<usize, Rc<Vec<usize>>>,
) -> Result<(), String> {
    let mut neighbors: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut seeds = Vec::new();
    for job in order {
        for (file, producer) in &planner.jobs[*job].dependencies {
            let output = planner.jobs[*producer]
                .output
                .values
                .iter()
                .find(|value| value.as_str() == Some(file));
            if output
                .is_some_and(|value| value.tag("pipe").is_some() || value.tag("service").is_some())
            {
                neighbors.entry(*job).or_default().push(*producer);
                neighbors.entry(*producer).or_default().push(*job);
                if output.is_some_and(|value| value.tag("pipe").is_some()) {
                    seeds.push(*producer);
                }
            }
        }
    }
    for seed in seeds {
        let mut members = HashSet::new();
        let mut pending = vec![seed];
        while let Some(job) = pending.pop() {
            if members.insert(job) {
                pending.extend(neighbors.get(&job).into_iter().flatten().copied());
                pending.extend(
                    groups
                        .get(&job)
                        .into_iter()
                        .flat_map(|jobs| jobs.iter())
                        .copied(),
                );
            }
        }
        if !members
            .iter()
            .any(|job| needed.get(job).copied().unwrap_or(false))
        {
            continue;
        }
        let mut external: Vec<_> = members
            .iter()
            .flat_map(|job| &planner.jobs[*job].dependencies)
            .map(|(_, job)| *job)
            .filter(|job| !members.contains(job))
            .collect();
        let mut seen = HashSet::new();
        while let Some(job) = external.pop() {
            if members.contains(&job) {
                return Err("Pipe dependencies leave and re-enter the component; these jobs cannot run together".into());
            }
            if seen.insert(job) {
                external.extend(planner.jobs[job].dependencies.iter().map(|(_, job)| *job));
            }
        }
        let ordered = Rc::new(
            order
                .iter()
                .copied()
                .filter(|job| members.contains(job))
                .collect::<Vec<_>>(),
        );
        for job in &members {
            needed.insert(*job, true);
            groups.insert(*job, ordered.clone());
        }
    }
    Ok(())
}

pub fn script(members: &[Task], pipes: &[String], services: &[String]) -> String {
    // Bash job control gives each member a process group, so failure also stops descendants.
    let mut script = String::from(
        "set -m\nstatus_dir=$(mktemp -d) || exit 1\npids=''\ntrap 'for pid in $pids; do kill -TERM -- -\"$pid\" 2>/dev/null || :; done; wait; rm -rf \"$status_dir\"' EXIT\ntrap 'exit 130' INT\ntrap 'exit 143' TERM\n",
    );
    for pipe in pipes {
        let parent = std::path::Path::new(pipe)
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| std::path::Path::new("."));
        script.push_str(&format!(
            "mkdir -p {} && rm -f {} && mkfifo {} || exit 1\n",
            quote(&parent.to_string_lossy()),
            quote(pipe),
            quote(pipe)
        ));
    }
    let connected = labels(members, pipes);
    let mut finite = Vec::new();
    for (number, task) in members.iter().enumerate() {
        let mut before = String::new();
        for (producer, other) in members.iter().enumerate() {
            for input in &task.inputs {
                if !other.outputs.contains(&input.path) || pipes.contains(&input.path) {
                    continue;
                }
                if services.contains(&input.path) {
                    before.push_str(&format!("while [ ! -e {} ] && [ ! -L {} ]; do [ ! -f \"$status_dir/{producer}\" ] || exit 1; sleep 0.1; done; ", quote(&input.path), quote(&input.path)));
                } else if connected[number] != connected[producer] {
                    before.push_str(&format!("while [ ! -f \"$status_dir/{producer}\" ]; do sleep 0.1; done; [ \"$(cat \"$status_dir/{producer}\")\" -eq 0 ] || exit 1; "));
                }
            }
        }
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
        script.push_str(&format!("( ( {before}{exports}exec {command} ); code=$?; printf '%s\\n' \"$code\" > \"$status_dir/{number}.part\"; mv \"$status_dir/{number}.part\" \"$status_dir/{number}\"; exit \"$code\" ) &\npids=\"$pids $!\"\n"));
        if !task.outputs.iter().any(|output| services.contains(output)) {
            finite.push(number);
        }
    }
    script.push_str("while :; do\n");
    for number in 0..members.len() {
        script.push_str(&format!("if [ -f \"$status_dir/{number}\" ]; then code=$(cat \"$status_dir/{number}\"); [ \"$code\" -eq 0 ] || exit \"$code\"; fi\n"));
    }
    let done = finite
        .iter()
        .map(|number| format!("[ -f \"$status_dir/{number}\" ]"))
        .collect::<Vec<_>>()
        .join(" && ");
    script.push_str(&format!("{done} && exit 0\nsleep 0.1\ndone\n"));
    script
}

fn labels(members: &[Task], pipes: &[String]) -> Vec<usize> {
    let mut labels: Vec<_> = (0..members.len()).collect();
    for (producer, task) in members.iter().enumerate() {
        for (consumer, other) in members.iter().enumerate() {
            if other
                .inputs
                .iter()
                .any(|input| pipes.contains(&input.path) && task.outputs.contains(&input.path))
            {
                let from = labels[consumer];
                let to = labels[producer];
                for label in &mut labels {
                    if *label == from {
                        *label = to;
                    }
                }
            }
        }
    }
    labels
}
