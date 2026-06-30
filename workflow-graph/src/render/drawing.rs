//! The drawings of the graph with one node per task and of the graph with one node per step.
//! In a simulated graph, each marked task or step gets a dashed box after it: more tasks may
//! follow from real data.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;

use super::{Drawing, Link, Shape, State};
use crate::steps::{StepGraph, StepNode};
use crate::{EdgeKind, ExecutionGraph, Language, MORE_TASKS, Marker, TaskId};

/// The most characters of a label in a box; longer names keep their end.
const LABEL: usize = 40;

impl ExecutionGraph {
    /// The drawing with one box per task.
    pub fn drawing(&self) -> Drawing {
        let simulated = self.is_simulated();
        let mut drawing = Drawing {
            title: title(
                &self.workflow,
                self.language,
                &format!(
                    "{} in {}",
                    count(self.nodes.len(), "task"),
                    count(self.steps().len(), "step")
                ),
                self.simulation.as_deref(),
                simulated && self.marked().next().is_some(),
            ),
            ..Drawing::default()
        };
        let mut index: HashMap<TaskId, usize> = HashMap::new();
        for node in &self.nodes {
            index.insert(node.task.id, drawing.shapes.len());
            let mut tooltip = vec![
                node.task.name.clone(),
                format!("step {}", node.task.step),
                match node.exit_code {
                    Some(0) if simulated => "simulated".to_owned(),
                    Some(code) => format!("exit code {code}"),
                    None => "not finished".to_owned(),
                },
            ];
            tooltip.extend(node.markers.iter().map(|marker| marker.reason.clone()));
            drawing.shapes.push(Shape {
                id: format!("t{}", node.task.id),
                lines: vec![shorten(&last_parts(&node.task.name, 1))],
                tooltip: tooltip.join("\n"),
                state: task_state(node.exit_code, simulated),
            });
        }
        for edge in &self.edges {
            let (Some(&from), Some(&to)) = (index.get(&edge.from), index.get(&edge.to)) else {
                continue;
            };
            drawing.links.push(match &edge.kind {
                EdgeKind::File { path } => Link {
                    from,
                    to,
                    dashed: false,
                    label: Some(path.clone()),
                },
                EdgeKind::Order => Link {
                    from,
                    to,
                    dashed: true,
                    label: None,
                },
            });
        }
        if simulated {
            for node in self.marked() {
                let after = index[&node.task.id];
                open_point(
                    &mut drawing,
                    after,
                    format!("m{}", node.task.id),
                    &node.markers,
                );
            }
        }
        drawing
    }
}

impl StepGraph {
    /// The drawing with one box per step, which counts its tasks.
    pub fn drawing(&self) -> Drawing {
        let simulated = self.is_simulated();
        let tasks: usize = self.steps.iter().map(|step| step.tasks).sum();
        let mut drawing = Drawing {
            title: title(
                &self.workflow,
                self.language,
                &format!(
                    "{} with {}",
                    count(self.steps.len(), "step"),
                    count(tasks, "task")
                ),
                self.simulation.as_deref(),
                simulated && self.steps.iter().any(|step| step.marked > 0),
            ),
            ..Drawing::default()
        };
        let mut index: HashMap<&str, usize> = HashMap::new();
        for (position, step) in self.steps.iter().enumerate() {
            index.insert(&step.name, position);
            let mut detail = count(step.tasks, "task");
            if step.failed > 0 {
                detail.push_str(&format!(", {} failed", step.failed));
            }
            let mut tooltip = vec![step.name.clone(), detail.clone()];
            if step.marked > 0 {
                tooltip.push(format!("{} with markers", count(step.marked, "task")));
            }
            tooltip.extend(step.markers.iter().map(|marker| marker.reason.clone()));
            drawing.shapes.push(Shape {
                id: format!("s{position}"),
                lines: vec![shorten(&last_parts(&step.name, 2)), detail],
                tooltip: tooltip.join("\n"),
                state: step_state(step, simulated),
            });
        }
        for edge in &self.edges {
            let (Some(&from), Some(&to)) =
                (index.get(edge.from.as_str()), index.get(edge.to.as_str()))
            else {
                continue;
            };
            drawing.links.push(Link {
                from,
                to,
                dashed: !edge.files,
                label: Some(count(edge.tasks, "task edge")),
            });
        }
        if simulated {
            for (position, step) in self.steps.iter().enumerate() {
                if step.marked > 0 {
                    open_point(
                        &mut drawing,
                        position,
                        format!("m{position}"),
                        &step.markers,
                    );
                }
            }
        }
        drawing
    }
}

/// The lines above a graph: the workflow and its size, the note of a simulation, and what the
/// dashed boxes mean when there are any.
fn title(
    workflow: &str,
    language: Option<Language>,
    size: &str,
    simulation: Option<&str>,
    open: bool,
) -> Vec<String> {
    // A folder given as `.` shows its name.
    let named = std::path::Path::new(workflow)
        .file_name()
        .is_none()
        .then(|| std::fs::canonicalize(workflow).ok())
        .flatten()
        .and_then(|path| Some(path.file_name()?.to_string_lossy().into_owned()));
    let workflow = named.as_deref().unwrap_or(workflow);
    let mut lines = vec![match language {
        Some(language) => format!("{workflow} ({language}): {size}"),
        None => format!("{workflow}: {size}"),
    }];
    lines.extend(simulation.map(str::to_owned));
    if open {
        lines.push(
            "Dashed boxes mark the points where a run with real data can add or remove tasks."
                .to_owned(),
        );
    }
    lines
}

/// Adds the dashed box after a marked task or step.
fn open_point(drawing: &mut Drawing, after: usize, id: String, markers: &[Marker]) {
    let reasons: Vec<&str> = markers
        .iter()
        .map(|marker| marker.reason.as_str())
        .collect();
    drawing.shapes.push(Shape {
        id,
        lines: vec![MORE_TASKS.to_owned()],
        tooltip: reasons.join("\n"),
        state: State::OpenPoint,
    });
    drawing.links.push(Link {
        from: after,
        to: drawing.shapes.len() - 1,
        dashed: true,
        label: None,
    });
}

fn task_state(exit_code: Option<i32>, simulated: bool) -> State {
    match exit_code {
        Some(0) if simulated => State::Simulated,
        Some(0) => State::Succeeded,
        Some(_) => State::Failed,
        None => State::Waiting,
    }
}

fn step_state(step: &StepNode, simulated: bool) -> State {
    if step.failed > 0 {
        State::Failed
    } else if step.succeeded < step.tasks {
        State::Waiting
    } else if simulated {
        State::Simulated
    } else {
        State::Succeeded
    }
}

/// `1 task`, `2 tasks`.
fn count(number: usize, noun: &str) -> String {
    if number == 1 {
        format!("1 {noun}")
    } else {
        format!("{number} {noun}s")
    }
}

/// A name qualified with `:`, like Nextflow's `NFCORE_DEMO:DEMO:FASTQC (SAMPLE1)`, with only
/// its last parts before the text after the first space: `FASTQC (SAMPLE1)` for one part. The
/// full name is in the tooltip.
fn last_parts(name: &str, parts: usize) -> String {
    let (qualified, rest) = match name.split_once(' ') {
        Some((qualified, rest)) => (qualified, Some(rest)),
        None => (name, None),
    };
    let names: Vec<&str> = qualified.split(':').collect();
    let kept = names[names.len().saturating_sub(parts)..].join(":");
    match rest {
        Some(rest) => format!("{kept} {rest}"),
        None => kept,
    }
}

/// A label of at most [`LABEL`] characters. A longer name with a tag after a space, like
/// `FASTQC (SAMPLE1)`, keeps its start, which names the step; any other keeps its end, which
/// tells most of a path or a qualified name.
pub(super) fn shorten(name: &str) -> String {
    let length = name.chars().count();
    if length <= LABEL {
        return name.to_owned();
    }
    if name.contains(' ') {
        let head: String = name.chars().take(LABEL - 1).collect();
        return format!("{}…", head.trim_end());
    }
    let separator = |character: char| matches!(character, ':' | '_' | '/' | '.');
    let skipped = length - (LABEL - 1);
    let tail: String = name.chars().skip(skipped).collect();
    // Start after a separator of the parts of the name, when one comes soon and the tail does
    // not start a part already.
    let whole = name.chars().nth(skipped - 1).is_some_and(separator);
    let start = tail
        .char_indices()
        .find(|(position, character)| !whole && separator(*character) && *position < LABEL / 2)
        .map_or(0, |(position, character)| position + character.len_utf8());
    format!("…{}", &tail[start..])
}

#[cfg(test)]
mod tests {
    use super::{last_parts, shorten};

    #[test]
    fn qualified_names_keep_their_last_parts() {
        let name = "NFCORE_RNASEQ:RNASEQ:FASTQ_QC:FQ_LINT (WT_REP1)";
        assert_eq!(last_parts(name, 1), "FQ_LINT (WT_REP1)");
        assert_eq!(
            last_parts("NFCORE_RNASEQ:RNASEQ:FASTQ_QC:FQ_LINT", 2),
            "FASTQ_QC:FQ_LINT"
        );
        assert_eq!(last_parts("bwa_map (sample=A)", 1), "bwa_map (sample=A)");
    }

    #[test]
    fn long_names_keep_their_end() {
        assert_eq!(shorten("FASTQC (a)"), "FASTQC (a)");
        let long = "NFCORE_RNASEQ:RNASEQ:FASTQ_FASTQC_UMITOOLS_TRIMGALORE:FASTQC";
        let short = shorten(long);
        assert!(short.chars().count() <= 40, "{short}");
        assert_eq!(short, "…FASTQ_FASTQC_UMITOOLS_TRIMGALORE:FASTQC");
        let tagged = shorten("CUSTOM_GTFFILTER (genes_with_empty_tid.filtered)");
        assert_eq!(tagged, "CUSTOM_GTFFILTER (genes_with_empty_tid.…");
    }
}
