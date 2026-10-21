//! Short texts of the graphs: the note of a simulation, counts, then one line per task or step
//! with what comes before it, and the markers under it.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::fmt::Write as _;

use crate::steps::StepGraph;
use crate::{ExecutionGraph, MORE_TASKS, Marker, TaskId};

/// What a line under a marked task or step starts with.
fn marker_prefix(simulated: bool) -> &'static str {
    if simulated {
        MORE_TASKS
    } else {
        "the next tasks depend on its outputs"
    }
}

/// The counts of markers for the first lines.
fn marked(count: usize, simulated: bool) -> String {
    match (count, simulated) {
        (0, _) => String::new(),
        (1, true) => ", 1 point where real data can add or remove tasks".to_owned(),
        (_, true) => format!(", {count} points where real data can add or remove tasks"),
        (_, false) => format!(", {count} marked"),
    }
}

fn markers(text: &mut String, markers: &[Marker], simulated: bool) {
    for marker in markers {
        let _ = writeln!(
            text,
            "      {}: {}",
            marker_prefix(simulated),
            marker.reason
        );
    }
}

/// One line per task: its id, its name and the tasks it comes after.
pub(super) fn tasks(graph: &ExecutionGraph) -> String {
    let simulated = graph.is_simulated();
    let mut text = String::new();
    if let Some(note) = &graph.simulation {
        let _ = writeln!(text, "{note}");
    }
    if let Some(note) = graph.cache_note() {
        let _ = writeln!(text, "{note}");
    }
    let _ = writeln!(
        text,
        "{} tasks, {} steps, {} edges{}",
        graph.nodes.len(),
        graph.steps().len(),
        graph.edges.len(),
        marked(graph.marked().count(), simulated)
    );
    let mut upstream: HashMap<TaskId, Vec<String>> = HashMap::new();
    for edge in &graph.edges {
        let from = edge.from.to_string();
        let list = upstream.entry(edge.to).or_default();
        if list.last() != Some(&from) {
            list.push(from);
        }
    }
    for node in &graph.nodes {
        let before = upstream
            .get(&node.task.id)
            .map(|list| list.join(", "))
            .unwrap_or_default();
        let cached = if node.reused { " (from the cache)" } else { "" };
        let _ = writeln!(
            text,
            "{:>5} {}{cached} <- [{before}]",
            node.task.id, node.task.name
        );
        markers(&mut text, &node.markers, simulated);
    }
    text
}

/// One line per step: its name, its tasks and the steps it comes after.
pub(super) fn steps(graph: &StepGraph) -> String {
    let simulated = graph.is_simulated();
    let mut text = String::new();
    if let Some(note) = &graph.simulation {
        let _ = writeln!(text, "{note}");
    }
    if let Some(note) = graph.cache_note() {
        let _ = writeln!(text, "{note}");
    }
    let tasks: usize = graph.steps.iter().map(|step| step.tasks).sum();
    let marked_steps = graph.steps.iter().filter(|step| step.marked > 0).count();
    let _ = writeln!(
        text,
        "{} steps, {} edges, {tasks} tasks{}",
        graph.steps.len(),
        graph.edges.len(),
        marked(marked_steps, simulated)
    );
    let mut upstream: HashMap<&str, Vec<&str>> = HashMap::new();
    for edge in &graph.edges {
        upstream.entry(&edge.to).or_default().push(&edge.from);
    }
    for step in &graph.steps {
        let mut line = format!(
            "{}: {} {}",
            step.name,
            step.tasks,
            if step.tasks == 1 { "task" } else { "tasks" }
        );
        if step.reused > 0 {
            let _ = write!(line, ", {} from the cache", step.reused);
        }
        if step.failed > 0 {
            let _ = write!(line, ", {} failed", step.failed);
        }
        let before = upstream
            .get(step.name.as_str())
            .map(|list| list.join(", "))
            .unwrap_or_default();
        let _ = writeln!(text, "{line} <- [{before}]");
        markers(&mut text, &step.markers, simulated);
    }
    text
}
