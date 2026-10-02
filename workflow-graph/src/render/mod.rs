//! Turns task and step graphs into drawings, SVG and text.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod dot;
mod drawing;
mod icons;
mod summary;
mod svg;

pub use crate::layout::Direction;
pub use dot::dot;
pub use svg::svg;

use std::collections::{BTreeSet, HashMap};

use crate::{ExecutionGraph, TaskId};

/// By default, a step with more tasks than this is one box in a picture of the graph of tasks.
pub const FOLD: usize = 5;

/// The most boxes side by side that a picture of the graph of tasks aims at.
/// See `docs/design/graph.md` for the full contract.
pub const WIDTH: usize = 16;

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
    /// Unique in the drawing: `t12` for task 12, `s3` for the fourth step in the order of
    /// their first task, `m12` for the open point after task 12 and `ms3` for the one after
    /// the fourth step. DOT and SVG use it as the id of the node.
    pub id: String,
    /// What the box stands for.
    pub kind: Kind,
    /// The lines of text in the box: the name, and for a step how many tasks it has.
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
    /// For the one line that stands for many lines into a box (more than [`WIDTH`]): how many
    /// task edges it stands for. It is drawn thick. 0 for any other line.
    pub bundled: usize,
    /// A line that stands under a bundle: it takes part in the layout, so that the box is
    /// laid out under all of its sources, but the picture does not draw it.
    pub hidden: bool,
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

    /// Its place in [`State::ALL`].
    fn index(self) -> usize {
        self as usize
    }
}

/// How to draw a graph.
#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    pub view: View,
    pub direction: Direction,
    /// In the graph of tasks, a step with more tasks than this is one box, unless it is open;
    /// 0 draws every task. A wide graph folds more steps (see [`WIDTH`]).
    pub fold: usize,
    /// The steps whose tasks are drawn although they have more than `fold`.
    pub open: BTreeSet<String>,
    /// The states of tasks where they are known better than the graph knows them, as the run
    /// database knows them while a run goes on. Other tasks get theirs from their exit codes.
    pub states: HashMap<TaskId, State>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            view: View::Tasks,
            direction: Direction::Down,
            fold: FOLD,
            open: BTreeSet::new(),
            states: HashMap::new(),
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

/// A graph in a format, in the view of the options. The pictures, DOT and SVG, are drawn as
/// the options say; JSON and the summary hold every task.
pub fn render(graph: &ExecutionGraph, options: &Options, format: Format) -> String {
    match (options.view, format) {
        (View::Tasks, Format::Json) => json(graph),
        (View::Tasks, Format::Summary) => summary::tasks(graph),
        (View::Steps, Format::Json) => json(&graph.step_graph()),
        (View::Steps, Format::Summary) => summary::steps(&graph.step_graph()),
        (_, Format::Dot) => dot(&graph.drawing(options), options.direction),
        (_, Format::Svg) => svg(&graph.drawing(options), options.direction),
    }
}

/// Pretty JSON of either graph, with a line end.
fn json(value: &impl serde::Serialize) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default() + "\n"
}

impl ExecutionGraph {
    /// The graph in Graphviz DOT form, one node per task, or per step with many tasks.
    pub fn to_dot(&self) -> String {
        render(self, &Options::default(), Format::Dot)
    }

    /// The graph as SVG, one node per task, or per step with many tasks.
    pub fn to_svg(&self) -> String {
        render(self, &Options::default(), Format::Svg)
    }

    /// A short text: the note of a simulation, counts, then one line per task with its
    /// upstream tasks and its markers.
    pub fn summary(&self) -> String {
        summary::tasks(self)
    }
}
