//! Pictures and texts of a graph. The graph with one node per task or one node per step
//! becomes a [`Drawing`]: boxes with text and a state, and lines between them. [`dot`] writes
//! a drawing as Graphviz DOT, and [`svg`] draws it as SVG with the layered layout of
//! [`crate::layout`], with no program besides iunctus. The web page of runs can draw any graph
//! the same way. [`render`] gives a graph in each view and format.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod dot;
mod drawing;
mod summary;
mod svg;

pub use crate::layout::Direction;
pub use dot::dot;
pub use svg::svg;

use crate::{ExecutionGraph, TaskId};

/// A graph to draw.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Drawing {
    /// Lines of text above the graph, like the workflow and the note of a simulation.
    pub title: Vec<String>,
    pub shapes: Vec<Shape>,
    pub links: Vec<Link>,
}

/// A box with text.
#[derive(Clone, Debug, PartialEq)]
pub struct Shape {
    /// Unique in the drawing, like `t12` for task 12, `s3` for the fourth step and `m12` for
    /// the open point after task 12. DOT and SVG use it as the id of the node.
    pub id: String,
    /// What the box stands for.
    pub kind: Kind,
    /// The lines of text in the box, the first one in bold for a step.
    pub lines: Vec<String>,
    /// More text, which a viewer shows when the pointer rests on the box.
    pub tooltip: String,
    pub state: State,
}

/// What a box stands for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    /// One task.
    Task(TaskId),
    /// The tasks of a step: a box of the graph of steps, or a step of the graph of tasks that
    /// is folded into one box.
    Step {
        /// The name of the step, as the tasks give it.
        name: String,
        /// How many tasks it has.
        tasks: usize,
    },
    /// The point after a marked task or step of a simulated graph, where more tasks may follow
    /// from real data. It is drawn beside the box before it.
    OpenPoint,
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

/// What a box shows about its task, or about the tasks of its step, by its icon.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum State {
    /// A task of a simulated run, which did not run.
    #[default]
    Simulated,
    Succeeded,
    /// Answered by an earlier result, from the cache store or from an earlier run, without
    /// running.
    Reused,
    Failed,
    Running,
    /// Asked for, and not finished; in a run that goes on, not started yet.
    Waiting,
    /// Stopped before it finished, as when its run ended.
    Stopped,
}

impl State {
    /// All states, in the order of the legend and of the counts of a step.
    pub const ALL: [State; 7] = [
        State::Simulated,
        State::Succeeded,
        State::Reused,
        State::Failed,
        State::Running,
        State::Waiting,
        State::Stopped,
    ];

    /// The name of the state, as the legend, the CSS classes and the web page name it.
    pub fn name(self) -> &'static str {
        match self {
            State::Simulated => "simulated",
            State::Succeeded => "succeeded",
            State::Reused => "reused",
            State::Failed => "failed",
            State::Running => "running",
            State::Waiting => "waiting",
            State::Stopped => "stopped",
        }
    }

    /// The main colour of the state, the colour of its icon.
    pub fn colour(self) -> &'static str {
        match self {
            State::Simulated => "#718096",
            State::Succeeded => "#2e7d4f",
            State::Reused => "#6b4fbb",
            State::Failed => "#c0392b",
            State::Running => "#1f6fd1",
            State::Waiting => "#8a939e",
            State::Stopped => "#b7791f",
        }
    }

    /// A light colour of the state, for the fill of a box of Graphviz.
    pub fn fill(self) -> &'static str {
        match self {
            State::Simulated => "#eef1f6",
            State::Succeeded => "#e6f4ea",
            State::Reused => "#efeafb",
            State::Failed => "#fdecea",
            State::Running => "#e8f0fe",
            State::Waiting => "#ffffff",
            State::Stopped => "#fdf3e1",
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
    Svg,
}

impl Format {
    /// The format that `-format` names: `json`, `dot`, `summary` or `svg`.
    pub fn parse(text: &str) -> Option<Format> {
        match text {
            "json" => Some(Format::Json),
            "dot" => Some(Format::Dot),
            "summary" => Some(Format::Summary),
            "svg" => Some(Format::Svg),
            _ => None,
        }
    }

    /// The format a file name asks for by its extension: DOT for `.dot`, SVG for `.svg`,
    /// a summary for `.txt`, else JSON.
    pub fn of_file(path: &std::path::Path) -> Format {
        match path.extension().and_then(|extension| extension.to_str()) {
            Some("dot") => Format::Dot,
            Some("svg") => Format::Svg,
            Some("txt") => Format::Summary,
            _ => Format::Json,
        }
    }
}

/// A graph in a view and a format.
pub fn render(graph: &ExecutionGraph, view: View, format: Format) -> String {
    match (view, format) {
        (View::Tasks, Format::Json) => json(graph),
        (View::Tasks, Format::Dot) => dot(&graph.drawing(), Direction::Down),
        (View::Tasks, Format::Summary) => summary::tasks(graph),
        (View::Tasks, Format::Svg) => svg(&graph.drawing()),
        (View::Steps, Format::Json) => json(&graph.step_graph()),
        (View::Steps, Format::Dot) => dot(&graph.step_graph().drawing(), Direction::Down),
        (View::Steps, Format::Summary) => summary::steps(&graph.step_graph()),
        (View::Steps, Format::Svg) => svg(&graph.step_graph().drawing()),
    }
}

/// Pretty JSON of either graph, with a line end.
fn json(value: &impl serde::Serialize) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default() + "\n"
}

impl ExecutionGraph {
    /// The graph in Graphviz DOT form, one node per task.
    pub fn to_dot(&self) -> String {
        dot(&self.drawing(), Direction::Down)
    }

    /// The graph as SVG, one node per task.
    pub fn to_svg(&self) -> String {
        svg(&self.drawing())
    }

    /// A short text: the note of a simulation, counts, then one line per task with its
    /// upstream tasks and its markers.
    pub fn summary(&self) -> String {
        summary::tasks(self)
    }
}
