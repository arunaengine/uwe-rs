//! Right-sized resources: what the tasks of each step used in earlier real runs of the
//! workflow sets the CPUs and memory that the next run asks for, and a note after the run says
//! what changed (see `docs/design/efficiency.md`).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use task_model::Measured;
use workflow_graph::ExecutionGraph;

use crate::arguments::Arguments;
use crate::clean::bytes;

/// What the tasks of each step used in earlier real runs.
pub type Measures = Arc<HashMap<String, Vec<Measured>>>;

/// What the tasks of each step of the workflow used in its earlier real runs. Empty with
/// `-no-right-sizing`, before the first real run, or without a run database.
pub fn measured(arguments: &Arguments) -> Measures {
    if arguments.no_right_sizing {
        return Measures::default();
    }
    let Some(workflow) = crate::recorded_workflow(arguments) else {
        return Measures::default();
    };
    let measured = (crate::runs::open(arguments).ok())
        .and_then(|database| database.measured(&workflow).ok())
        .unwrap_or_default();
    Arc::new(measured)
}

/// The requests of the tasks of one step, as the workflow asks and as right-sized.
#[derive(Default)]
struct Step {
    tasks: usize,
    asked_cpus: u32,
    cpus: u32,
    asked_memory: u64,
    memory: u64,
}

/// The lines that say which steps asked for less than their workflow: for each such step, the
/// most CPUs and memory that one of its tasks asked for, before and after. A simulated run
/// says what a real run would ask for. Tasks that came from the cache asked for nothing.
pub fn notes(graph: &ExecutionGraph, measured: &Measures, simulated: bool) -> Vec<String> {
    let mut steps: BTreeMap<&str, Step> = BTreeMap::new();
    for node in graph.nodes.iter().filter(|node| !node.reused) {
        let task = &node.task;
        let Some(earlier) = measured.get(&task.step) else {
            continue;
        };
        let asked = task_runner::request(task, None, Vec::new(), None).placement;
        let sized = task_runner::resources::placement(task, &asked, 1, earlier);
        if sized == asked {
            continue;
        }
        let step = steps.entry(&task.step).or_default();
        step.tasks += 1;
        step.asked_cpus = step.asked_cpus.max(asked.cpus);
        step.cpus = step.cpus.max(sized.cpus);
        step.asked_memory = step
            .asked_memory
            .max(asked.memory_bytes.unwrap_or_default());
        step.memory = step.memory.max(sized.memory_bytes.unwrap_or_default());
    }
    if steps.is_empty() {
        return Vec::new();
    }
    let first = match simulated {
        true => {
            "iunctus: a real run would ask for less than the workflow, from what earlier \
                 runs used (-no-right-sizing keeps what the workflow asks for):"
        }
        false => {
            "iunctus: tasks asked for less than the workflow, from what earlier runs used \
                  (-no-right-sizing keeps what the workflow asks for):"
        }
    };
    let mut lines = vec![first.to_owned()];
    for (name, step) in steps {
        let mut changes = Vec::new();
        if step.cpus < step.asked_cpus {
            changes.push(format!(
                "{} instead of {}",
                cpus(step.cpus),
                cpus(step.asked_cpus)
            ));
        }
        if step.memory < step.asked_memory {
            changes.push(format!(
                "{} of memory instead of {}",
                bytes(step.memory),
                bytes(step.asked_memory)
            ));
        }
        let tasks = match step.tasks {
            1 => "1 task".to_owned(),
            count => format!("{count} tasks"),
        };
        lines.push(format!(
            "iunctus:   {name} ({tasks}): {}",
            changes.join(", ")
        ));
    }
    lines
}

fn cpus(count: u32) -> String {
    match count {
        1 => "1 CPU".to_owned(),
        count => format!("{count} CPUs"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use workflow_graph::{Node, Resources, Task};

    fn node(step: &str, reused: bool) -> Node {
        Node {
            task: Task {
                step: step.into(),
                resources: Resources {
                    cpus: Some(4.0),
                    memory_bytes: Some(8_000_000_000),
                    ..Resources::default()
                },
                ..Task::default()
            },
            exit_code: Some(0),
            reused,
            markers: Vec::new(),
        }
    }

    #[test]
    fn the_note_names_the_steps_that_asked_for_less() {
        let graph = ExecutionGraph {
            nodes: vec![
                node("ALIGN", false),
                node("ALIGN", false),
                node("SORT", true),
            ],
            ..ExecutionGraph::default()
        };
        let used = Measured {
            memory_peak: Some(800_000_000),
            cpu_usec: Some(1_000_000),
            milliseconds: 1_000,
            input_bytes: None,
            overhead: None,
        };
        let measured: Measures = Arc::new(
            [
                ("ALIGN".to_owned(), vec![used]),
                ("SORT".to_owned(), vec![used]),
            ]
            .into(),
        );
        let lines = notes(&graph, &measured, false);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert_eq!(
            lines[1],
            "iunctus:   ALIGN (2 tasks): 2 CPUs instead of 4 CPUs, 1.0 GB of memory instead \
             of 8.0 GB"
        );
        assert!(notes(&graph, &Measures::default(), true).is_empty());
    }
}
