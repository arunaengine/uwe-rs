//! Pictures and texts of a graph. The graph with one node per task or one node per step
//! becomes a [`Drawing`]: boxes with text and a state, and lines between them. [`dot`] writes
//! a drawing as Graphviz DOT. [`render`] gives a graph in each view and format.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod dot;
mod drawing;
mod summary;

pub use dot::dot;

use crate::ExecutionGraph;

/// A graph to draw.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Drawing {
    /// Lines of text above the graph, like the workflow and the note of a simulation.
    pub title: Vec<String>,
    pub shapes: Vec<Shape>,
    pub links: Vec<Link>,
}

/// A box with text.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Shape {
    /// Unique in the drawing, like `t12` for task 12, `s3` for the fourth step and `m12` for
    /// the open point after task 12. DOT and SVG use it as the id of the node.
    pub id: String,
    /// The lines of text in the box, the first one in bold for a step.
    pub lines: Vec<String>,
    /// More text, which a viewer shows when the pointer rests on the box.
    pub tooltip: String,
    pub state: State,
}

/// A line from one box to another, by their positions in [`Drawing::shapes`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Link {
    pub from: usize,
    pub to: usize,
    /// A dashed line: the second box only waits for the first, or it is an open point.
    pub dashed: bool,
    /// Text for the line, like the file it passes: DOT writes it next to the line, and SVG
    /// shows it when the pointer rests on the line.
    pub label: Option<String>,
}

/// What a box shows about its task or step, by its colour.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum State {
    /// A task of a simulated run, which did not run.
    #[default]
    Simulated,
    Succeeded,
    Failed,
    /// Answered from the cache store without running.
    FromCache,
    /// Asked for, but not finished.
    Waiting,
    /// The point after a marked task or step, where more tasks may follow from real data.
    OpenPoint,
}

impl State {
    /// All states, in the order of the legend.
    pub const ALL: [State; 6] = [
        State::Simulated,
        State::Succeeded,
        State::FromCache,
        State::Failed,
        State::Waiting,
        State::OpenPoint,
    ];

    /// The name of the state in the legend and as a CSS class.
    pub fn name(self) -> &'static str {
        match self {
            State::Simulated => "simulated",
            State::Succeeded => "succeeded",
            State::Failed => "failed",
            State::FromCache => "from cache",
            State::Waiting => "not finished",
            State::OpenPoint => crate::MORE_TASKS,
        }
    }

    /// The fill and border colours.
    pub fn colours(self) -> (&'static str, &'static str) {
        match self {
            State::Simulated => ("#eef1f6", "#718096"),
            State::Succeeded => ("#e6f4ea", "#2f855a"),
            State::Failed => ("#fdecea", "#c53030"),
            State::FromCache => ("#e8f0fe", "#2b6cb0"),
            State::Waiting => ("#ffffff", "#a0aec0"),
            State::OpenPoint => ("#fff8e1", "#b7791f"),
        }
    }
}

/// Which graph to show: one node per task, or one node per step.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum View {
    #[default]
    Tasks,
    Steps,
}

impl View {
    /// The view that `-view` names: `tasks` or `steps`.
    pub fn parse(text: &str) -> Option<View> {
        match text {
            "tasks" => Some(View::Tasks),
            "steps" => Some(View::Steps),
            _ => None,
        }
    }
}

/// How to write a graph.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Format {
    Json,
    Dot,
    #[default]
    Summary,
}

impl Format {
    /// The format that `-format` names: `json`, `dot` or `summary`.
    pub fn parse(text: &str) -> Option<Format> {
        match text {
            "json" => Some(Format::Json),
            "dot" => Some(Format::Dot),
            "summary" => Some(Format::Summary),
            _ => None,
        }
    }

    /// The format a file name asks for by its extension: DOT for `.dot`, a summary for
    /// `.txt`, else JSON.
    pub fn of_file(path: &std::path::Path) -> Format {
        match path.extension().and_then(|extension| extension.to_str()) {
            Some("dot") => Format::Dot,
            Some("txt") => Format::Summary,
            _ => Format::Json,
        }
    }
}

/// A graph in a view and a format.
pub fn render(graph: &ExecutionGraph, view: View, format: Format) -> String {
    match (view, format) {
        (View::Tasks, Format::Json) => json(graph),
        (View::Tasks, Format::Dot) => dot(&graph.drawing()),
        (View::Tasks, Format::Summary) => summary::tasks(graph),
        (View::Steps, Format::Json) => json(&graph.step_graph()),
        (View::Steps, Format::Dot) => dot(&graph.step_graph().drawing()),
        (View::Steps, Format::Summary) => summary::steps(&graph.step_graph()),
    }
}

/// Pretty JSON of either graph, with a line end.
fn json(value: &impl serde::Serialize) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default() + "\n"
}

impl ExecutionGraph {
    /// The graph in Graphviz DOT form, one node per task.
    pub fn to_dot(&self) -> String {
        dot(&self.drawing())
    }

    /// A short text: the note of a simulation, counts, then one line per task with its
    /// upstream tasks and its markers.
    pub fn summary(&self) -> String {
        summary::tasks(self)
    }
}
