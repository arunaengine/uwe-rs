//! Builds task and step boxes, graph edges and open points for rendering.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, HashMap};

use super::{Drawing, Kind, Link, Options, Shape, State, View, WIDTH};
use crate::{EdgeKind, ExecutionGraph, Language, MORE_TASKS, Marker, Node, TaskId};

/// The most characters of a label in a box; longer names keep their end.
const LABEL: usize = 40;

/// How many tasks are in each state, in the order of [`State::ALL`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Tally([usize; State::ALL.len()]);

impl Tally {
    fn add(&mut self, state: State) {
        self.0[state.index()] += 1;
    }

    fn total(&self) -> usize {
        self.0.iter().sum()
    }

    /// The state that a box of these tasks shows: a failure first, then work that goes on,
    /// then how the tasks ended.
    pub(crate) fn state(&self) -> State {
        let order = [
            State::Failed,
            State::Running,
            State::Waiting,
            State::Stopped,
            State::Succeeded,
            State::Simulated,
        ];
        let found = order.into_iter().find(|state| self.0[state.index()] > 0);
        found.unwrap_or(State::Reused)
    }

    /// `24 tasks: 20 succeeded, 4 running`, in the order of [`State::ALL`].
    fn spelled(&self) -> String {
        let states: Vec<String> = (State::ALL.iter())
            .filter(|state| self.0[state.index()] > 0)
            .map(|state| format!("{} {}", self.0[state.index()], state.name()))
            .collect();
        format!("{}: {}", count(self.total(), "task"), states.join(", "))
    }

    /// `24 tasks` when every task is in the state of the box, else as [`Tally::spelled`].
    fn short(&self) -> String {
        if self.0[self.state().index()] == self.total() {
            count(self.total(), "task")
        } else {
            self.spelled()
        }
    }
}

/// A step of the graph, with its tasks.
struct Step<'a> {
    name: &'a str,
    /// The positions of its tasks in the nodes of the graph.
    tasks: Vec<usize>,
    tally: Tally,
    /// Whether it is one box.
    folded: bool,
}

/// The lines between two boxes, gathered: how many task edges they stand for, and whether one
/// passes a file, with the first file.
struct Join {
    from: usize,
    to: usize,
    tasks: usize,
    file: Option<String>,
}

impl ExecutionGraph {
    /// The drawing of the graph in the view of the options: a box per task, where a step with
    /// more tasks than `options.fold` is one box unless it is open, or a box per step.
    pub fn drawing(&self, options: &Options) -> Drawing {
        let simulated = self.is_simulated();
        let states: Vec<State> = (self.nodes.iter())
            .map(|node| match options.states.get(&node.task.id) {
                Some(state) => *state,
                None => node_state(node, simulated),
            })
            .collect();
        let (steps, fold) = self.steps_of(&states, options);
        let mut drawing = Drawing {
            title: self.title(&steps, options, fold),
            ..Drawing::default()
        };
        // The box of each task, and of each folded step; and the markers of each box.
        let mut shape_of = vec![0; self.nodes.len()];
        let mut step_shapes: Vec<Option<usize>> = vec![None; steps.len()];
        let mut marked: Vec<(usize, String, Vec<Marker>)> = Vec::new();
        let mut step_at: HashMap<&str, usize> = HashMap::new();
        for (at, step) in steps.iter().enumerate() {
            step_at.insert(step.name, at);
        }
        for (position, node) in self.nodes.iter().enumerate() {
            let at = step_at[node.task.step.as_str()];
            let step = &steps[at];
            if !step.folded {
                shape_of[position] = drawing.shapes.len();
                if !node.markers.is_empty() {
                    let open = format!("m{}", node.task.id);
                    marked.push((drawing.shapes.len(), open, node.markers.clone()));
                }
                drawing.shapes.push(task_shape(node, states[position]));
                continue;
            }
            shape_of[position] = *step_shapes[at].get_or_insert_with(|| {
                let markers = self.step_markers(step);
                let shape = step_shape(at, step, &markers);
                if !markers.is_empty() {
                    marked.push((drawing.shapes.len(), format!("ms{at}"), markers));
                }
                drawing.shapes.push(shape);
                drawing.shapes.len() - 1
            });
        }
        drawing.links = self.links(&drawing.shapes, &shape_of);
        if simulated {
            for (after, id, markers) in marked {
                open_point(&mut drawing, after, id, &markers);
            }
        }
        drawing
    }

    /// The steps in the order of their first task, whether each is one box, and the most tasks
    /// that a step may have to be drawn task by task.
    fn steps_of(&self, states: &[State], options: &Options) -> (Vec<Step<'_>>, usize) {
        let mut steps: Vec<Step> = Vec::new();
        let mut index: HashMap<&str, usize> = HashMap::new();
        for (position, node) in self.nodes.iter().enumerate() {
            let name = node.task.step.as_str();
            let at = *index.entry(name).or_insert_with(|| {
                steps.push(Step {
                    name,
                    tasks: Vec::new(),
                    tally: Tally::default(),
                    folded: false,
                });
                steps.len() - 1
            });
            steps[at].tasks.push(position);
            steps[at].tally.add(states[position]);
        }
        let fold = match options.view {
            View::Steps => 0,
            View::Tasks => self.fold_for(&steps, options),
        };
        for step in &mut steps {
            step.folded = match options.view {
                View::Steps => true,
                View::Tasks => {
                    fold > 0 && step.tasks.len() > fold && !options.open.contains(step.name)
                }
            };
        }
        (steps, fold)
    }

    /// Chooses task folding to respect `options.fold` and graph width; open steps stay open.
    /// See `docs/design/graph.md` for the full contract.
    fn fold_for(&self, steps: &[Step], options: &Options) -> usize {
        if options.fold == 0 {
            return 0;
        }
        let depths = self.depths();
        let rows = depths.iter().max().map_or(0, |deepest| deepest + 1);
        // In which rows each step has tasks, and how many.
        let spans: Vec<Vec<(usize, usize)>> = (steps.iter())
            .map(|step| {
                let mut found: BTreeMap<usize, usize> = BTreeMap::new();
                for &position in &step.tasks {
                    *found.entry(depths[position]).or_default() += 1;
                }
                found.into_iter().collect()
            })
            .collect();
        let mut narrowest = (usize::MAX, options.fold);
        for fold in (1..=options.fold).rev() {
            let mut widths = vec![0; rows];
            for (step, span) in steps.iter().zip(&spans) {
                let drawn = step.tasks.len() <= fold || options.open.contains(step.name);
                for &(row, count) in span {
                    widths[row] += if drawn { count } else { 1 };
                }
            }
            let widest = widths.into_iter().max().unwrap_or(0);
            if widest <= WIDTH {
                return fold;
            }
            if widest < narrowest.0 {
                narrowest = (widest, fold);
            }
        }
        narrowest.1
    }

    /// The row of each task: the length of the longest chain of tasks before it.
    fn depths(&self) -> Vec<usize> {
        let positions = self.positions();
        let mut waiting = vec![0; self.nodes.len()];
        let mut after: Vec<Vec<usize>> = vec![Vec::new(); self.nodes.len()];
        for edge in &self.edges {
            if let (Some(&from), Some(&to)) = (positions.get(&edge.from), positions.get(&edge.to)) {
                after[from].push(to);
                waiting[to] += 1;
            }
        }
        let mut depths = vec![0; self.nodes.len()];
        let mut ready: Vec<usize> = (0..self.nodes.len())
            .filter(|&position| waiting[position] == 0)
            .collect();
        while let Some(position) = ready.pop() {
            for &next in &after[position] {
                depths[next] = depths[next].max(depths[position] + 1);
                waiting[next] -= 1;
                if waiting[next] == 0 {
                    ready.push(next);
                }
            }
        }
        depths
    }

    /// The position of each task in the nodes.
    fn positions(&self) -> HashMap<TaskId, usize> {
        (self.nodes.iter().enumerate())
            .map(|(position, node)| (node.task.id, position))
            .collect()
    }

    /// The markers of the tasks of a step, one per kind, each with the reason of the first
    /// task that has it.
    fn step_markers(&self, step: &Step) -> Vec<Marker> {
        let mut markers: Vec<Marker> = Vec::new();
        for &position in &step.tasks {
            for marker in &self.nodes[position].markers {
                if !markers.iter().any(|known| known.kind == marker.kind) {
                    markers.push(marker.clone());
                }
            }
        }
        markers
    }

    /// The lines between the boxes: one per pair of boxes that task edges join, dashed when no
    /// task of the second reads a file of the first. Edges inside a folded step are left out.
    /// More than [`WIDTH`] lines into one box are bundled into one.
    fn links(&self, shapes: &[Shape], shape_of: &[usize]) -> Vec<Link> {
        let positions = self.positions();
        let mut joins: Vec<Join> = Vec::new();
        let mut index: HashMap<(usize, usize), usize> = HashMap::new();
        for edge in &self.edges {
            let (Some(&from), Some(&to)) = (positions.get(&edge.from), positions.get(&edge.to))
            else {
                continue;
            };
            let (from, to) = (shape_of[from], shape_of[to]);
            if from == to {
                continue;
            }
            let at = *index.entry((from, to)).or_insert_with(|| {
                joins.push(Join {
                    from,
                    to,
                    tasks: 0,
                    file: None,
                });
                joins.len() - 1
            });
            joins[at].tasks += 1;
            if let EdgeKind::File { path } = &edge.kind {
                joins[at].file.get_or_insert_with(|| path.clone());
            }
        }
        let task = |shape: usize| matches!(shapes[shape].kind, Kind::Task(_));
        let mut links: Vec<Link> = (joins.iter())
            .map(|join| Link {
                from: join.from,
                to: join.to,
                dashed: join.file.is_none(),
                label: if task(join.from) && task(join.to) {
                    join.file.clone()
                } else {
                    Some(count(join.tasks, "task edge"))
                },
                ..Link::default()
            })
            .collect();
        bundle(&joins, &mut links);
        links
    }

    /// The lines above the graph, and which steps are one box.
    fn title(&self, steps: &[Step], options: &Options, fold: usize) -> Vec<String> {
        let (tasks, counted) = (count(self.nodes.len(), "task"), count(steps.len(), "step"));
        let size = match options.view {
            View::Tasks => format!("{tasks} in {counted}"),
            View::Steps => format!("{counted} with {tasks}"),
        };
        let open = self.is_simulated() && self.marked().next().is_some();
        let simulation = self.simulation.as_deref();
        let mut lines = title(&self.workflow, self.language, &size, simulation, open);
        // What came from the cache, or would in a real run, right after the note of a
        // simulation.
        if let Some(note) = self.cache_note() {
            let at = 1 + usize::from(simulation.is_some());
            lines.insert(at.min(lines.len()), note);
        }
        if options.view == View::Tasks && steps.iter().any(|step| step.folded) {
            let many = count(fold, "task");
            lines.push(if fold < options.fold {
                format!("The graph is wide: each step with more than {many} is one box.")
            } else {
                format!("Each step with more than {many} is one box.")
            });
        }
        lines
    }
}

/// Bundles the lines into each box that more than [`WIDTH`] lines run into: the line of the
/// middle source, in the order the tasks were asked for, stays and stands for all of them, and
/// the others are hidden, so that the layout still puts the box under all of its sources.
fn bundle(joins: &[Join], links: &mut [Link]) {
    let mut into: HashMap<usize, Vec<usize>> = HashMap::new();
    for (at, join) in joins.iter().enumerate() {
        into.entry(join.to).or_default().push(at);
    }
    for mut group in into.into_values().filter(|group| group.len() > WIDTH) {
        group.sort_by_key(|&at| joins[at].from);
        let middle = group[group.len() / 2];
        let tasks: usize = group.iter().map(|&at| joins[at].tasks).sum();
        let dashed = group.iter().all(|&at| links[at].dashed);
        for &at in &group {
            links[at].hidden = at != middle;
        }
        links[middle].bundled = tasks;
        links[middle].dashed = dashed;
        links[middle].label = Some(count(tasks, "task edge"));
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

/// The box of a task.
fn task_shape(node: &Node, state: State) -> Shape {
    let what = match (state, node.exit_code) {
        (State::Succeeded | State::Failed, Some(code)) => format!("exit code {code}"),
        (State::Reused, _) => "reused an earlier result".to_owned(),
        (state, _) => state.name().to_owned(),
    };
    let mut tooltip = vec![
        node.task.name.clone(),
        format!("step {}", node.task.step),
        what,
    ];
    tooltip.extend(node.markers.iter().map(|marker| marker.reason.clone()));
    Shape {
        id: format!("t{}", node.task.id),
        kind: Kind::Task(node.task.id),
        lines: vec![shorten(&last_parts(&node.task.name, 1))],
        tooltip: tooltip.join("\n"),
        state,
    }
}

/// The box of a step: its name, and how many tasks it has in which states.
fn step_shape(at: usize, step: &Step, markers: &[Marker]) -> Shape {
    let mut tooltip = vec![step.name.to_owned(), step.tally.spelled()];
    if !markers.is_empty() {
        tooltip.extend(markers.iter().map(|marker| marker.reason.clone()));
    }
    Shape {
        id: format!("s{at}"),
        kind: Kind::Step {
            name: step.name.to_owned(),
            tasks: step.tasks.len(),
        },
        lines: vec![shorten(&last_parts(step.name, 2)), step.tally.short()],
        tooltip: tooltip.join("\n"),
        state: step.tally.state(),
    }
}

/// Adds the dashed box after a marked task or step.
fn open_point(drawing: &mut Drawing, after: usize, id: String, markers: &[Marker]) {
    let mut tooltip = vec![MORE_TASKS.to_owned()];
    tooltip.extend(markers.iter().map(|marker| marker.reason.clone()));
    drawing.shapes.push(Shape {
        id,
        kind: Kind::OpenPoint,
        lines: vec!["more tasks may follow".to_owned()],
        tooltip: tooltip.join("\n"),
        state: State::default(),
    });
    drawing.links.push(Link {
        from: after,
        to: drawing.shapes.len() - 1,
        dashed: true,
        ..Link::default()
    });
}

/// The state of a task from native acceptance, or its exit code when acceptance is unknown.
fn node_state(node: &Node, simulated: bool) -> State {
    match node.exit_code {
        Some(code) if !node.accepted.unwrap_or(code == 0) => State::Failed,
        Some(_) if node.reused => State::Reused,
        Some(_) if simulated => State::Simulated,
        Some(_) => State::Succeeded,
        None => State::Waiting,
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
    use super::*;
    use crate::{Edge, MarkerKind, Task};

    /// INPUT, then seven FASTQC tasks that read its file, then MULTIQC that reads theirs.
    fn fan_out() -> ExecutionGraph {
        let task = |id: u64, step: &str| Node {
            task: Task {
                id: TaskId(id),
                name: format!("{step} ({id})"),
                step: step.into(),
                ..Task::default()
            },
            exit_code: Some(0),
            accepted: None,
            reused: false,
            markers: Vec::new(),
        };
        let mut nodes = vec![task(1, "INPUT")];
        nodes.extend((2..=8).map(|id| task(id, "FASTQC")));
        nodes.push(task(9, "MULTIQC"));
        let file = |from: u64, to: u64| Edge {
            from: TaskId(from),
            to: TaskId(to),
            kind: EdgeKind::File {
                path: format!("{from}.txt"),
            },
        };
        let mut edges: Vec<Edge> = (2..=8).map(|id| file(1, id)).collect();
        edges.extend((2..=8).map(|id| file(id, 9)));
        ExecutionGraph {
            workflow: "demo".into(),
            nodes,
            edges,
            ..ExecutionGraph::default()
        }
    }

    fn ids(drawing: &Drawing) -> Vec<&str> {
        drawing
            .shapes
            .iter()
            .map(|shape| shape.id.as_str())
            .collect()
    }

    #[test]
    fn folds_large_steps() {
        let mut graph = fan_out();
        graph.nodes[3].exit_code = Some(1);
        graph.nodes[4].exit_code = None;
        let drawing = graph.drawing(&Options::default());
        assert_eq!(ids(&drawing), ["t1", "s1", "t9"]);
        let step = &drawing.shapes[1];
        let kind = Kind::Step {
            name: "FASTQC".into(),
            tasks: 7,
        };
        assert_eq!(step.kind, kind);
        assert_eq!(
            step.lines,
            ["FASTQC", "7 tasks: 5 succeeded, 1 failed, 1 waiting"]
        );
        assert_eq!(step.state, State::Failed);
        let links: Vec<(usize, usize, Option<&str>)> = (drawing.links.iter())
            .map(|link| (link.from, link.to, link.label.as_deref()))
            .collect();
        let edges = Some("7 task edges");
        assert_eq!(links, [(0, 1, edges), (1, 2, edges)]);
        let note = "Each step with more than 5 tasks is one box.";
        assert!(drawing.title.iter().any(|line| line == note));
        // An open step shows its tasks, and 0 folds none.
        let open = Options {
            open: ["FASTQC".to_owned()].into(),
            ..Options::default()
        };
        assert_eq!(graph.drawing(&open).shapes.len(), 9);
        let every = graph.drawing(&Options {
            fold: 0,
            ..Options::default()
        });
        assert_eq!(every.shapes.len(), 9);
        assert_eq!(every.links[0].label.as_deref(), Some("1.txt"));
    }

    #[test]
    fn counts_cached_tasks() {
        let mut graph = fan_out();
        for node in &mut graph.nodes[..6] {
            node.reused = true;
        }
        let real = "6 of 9 tasks came from the cache.";
        assert_eq!(graph.cache_note().as_deref(), Some(real));
        graph.simulate();
        let preview = "A real run would take 6 of 9 tasks from the cache; 3 would run.";
        assert_eq!(graph.cache_note().as_deref(), Some(preview));
        let drawing = graph.drawing(&Options::default());
        assert_eq!(drawing.title[2], preview);
        let step = &drawing.shapes[1];
        assert_eq!(step.lines, ["FASTQC", "7 tasks: 2 simulated, 5 reused"]);
        let summary = graph.summary();
        assert!(summary.contains(&format!("{preview}\n")), "{summary}");
        assert!(
            summary.contains("1 INPUT (1) (from the cache) <- []"),
            "{summary}"
        );
        assert!(summary.contains("9 MULTIQC (9) <- [2, 3, 4"), "{summary}");
        let by_step = Options {
            view: View::Steps,
            ..Options::default()
        };
        let steps = crate::render::render(&graph, &by_step, crate::render::Format::Summary);
        assert!(
            steps.contains("FASTQC: 7 tasks, 5 from the cache <- [INPUT]"),
            "{steps}"
        );
        assert_eq!(graph.step_graph().cache_note().as_deref(), Some(preview));
        for node in &mut graph.nodes {
            node.reused = false;
        }
        assert_eq!(graph.cache_note(), None);
    }

    #[test]
    fn uses_native_acceptance() {
        let mut graph = fan_out();
        graph.nodes[0].exit_code = Some(7);
        graph.nodes[0].accepted = Some(true);
        graph.nodes[1].exit_code = Some(7);
        graph.nodes[1].accepted = Some(true);
        graph.nodes[1].reused = true;
        graph.nodes[2].accepted = Some(false);

        let steps = graph.step_graph();
        assert_eq!((steps.steps[0].succeeded, steps.steps[0].failed), (1, 0));
        assert_eq!(
            (
                steps.steps[1].succeeded,
                steps.steps[1].failed,
                steps.steps[1].reused
            ),
            (6, 1, 1)
        );
        let drawing = graph.drawing(&Options {
            fold: 0,
            ..Options::default()
        });
        assert_eq!(drawing.shapes[0].state, State::Succeeded);
        assert_eq!(drawing.shapes[1].state, State::Reused);
        assert_eq!(drawing.shapes[2].state, State::Failed);
    }

    /// One SOURCE task, then `steps` steps of `tasks` tasks each that read its file, then SINK,
    /// which reads a file of every task.
    fn parallel_steps(steps: usize, tasks: usize) -> ExecutionGraph {
        let task = |id: u64, step: String| Node {
            task: Task {
                id: TaskId(id),
                name: format!("{step} ({id})"),
                step,
                ..Task::default()
            },
            exit_code: Some(0),
            accepted: None,
            reused: false,
            markers: Vec::new(),
        };
        let mut nodes = vec![task(1, "SOURCE".into())];
        let mut edges = Vec::new();
        let last = 2 + (steps * tasks) as u64;
        for at in 0..(steps * tasks) as u64 {
            let id = 2 + at;
            nodes.push(task(id, format!("STEP{}", at as usize / tasks)));
            for (from, to) in [(1, id), (id, last)] {
                let kind = EdgeKind::File {
                    path: format!("{from}.txt"),
                };
                let (from, to) = (TaskId(from), TaskId(to));
                edges.push(Edge { from, to, kind });
            }
        }
        nodes.push(task(last, "SINK".into()));
        ExecutionGraph {
            workflow: "demo".into(),
            nodes,
            edges,
            ..ExecutionGraph::default()
        }
    }

    #[test]
    fn folds_wide_graphs() {
        // Three steps of four tasks stand in a row of 12: every task has its box.
        let narrow = parallel_steps(3, 4).drawing(&Options::default());
        assert_eq!(narrow.shapes.len(), 14);
        // Six steps of four tasks would stand in a row of 24: each step is one box.
        let graph = parallel_steps(6, 4);
        let wide = graph.drawing(&Options::default());
        assert_eq!(wide.shapes.len(), 8);
        let note = "The graph is wide: each step with more than 3 tasks is one box.";
        assert!(
            wide.title.iter().any(|line| line == note),
            "{:?}",
            wide.title
        );
        // A step that is open keeps its tasks, and 0 folds none.
        let open = Options {
            open: ["STEP0".to_owned()].into(),
            ..Options::default()
        };
        assert_eq!(graph.drawing(&open).shapes.len(), 11);
        let every = Options {
            fold: 0,
            ..Options::default()
        };
        assert_eq!(graph.drawing(&every).shapes.len(), 26);
    }

    #[test]
    fn counts_merged_edges() {
        // Twenty tasks of an open step read by one task: one thick line, nineteen hidden.
        let graph = parallel_steps(1, 20);
        let open = Options {
            open: ["STEP0".to_owned()].into(),
            ..Options::default()
        };
        let drawing = graph.drawing(&open);
        let into_sink: Vec<&Link> = (drawing.links.iter())
            .filter(|link| link.to == drawing.shapes.len() - 1)
            .collect();
        assert_eq!(into_sink.len(), 20);
        let shown: Vec<&&Link> = into_sink.iter().filter(|link| !link.hidden).collect();
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].bundled, 20);
        assert_eq!(shown[0].label.as_deref(), Some("20 task edges"));
        // Fewer lines stay as they are.
        let few = parallel_steps(1, 16).drawing(&open);
        assert!(
            few.links
                .iter()
                .all(|link| !link.hidden && link.bundled == 0)
        );
    }

    #[test]
    fn boxes_each_step() {
        let options = Options {
            view: View::Steps,
            ..Options::default()
        };
        let drawing = fan_out().drawing(&options);
        let lines: Vec<Vec<&str>> = (drawing.shapes.iter())
            .map(|shape| shape.lines.iter().map(String::as_str).collect())
            .collect();
        assert_eq!(
            lines,
            [
                ["INPUT", "1 task"],
                ["FASTQC", "7 tasks"],
                ["MULTIQC", "1 task"]
            ]
        );
        assert_eq!(drawing.shapes[1].state, State::Succeeded);
        assert_eq!(drawing.title[0], "demo: 3 steps with 9 tasks");
    }

    #[test]
    fn preserves_known_states() {
        let mut graph = fan_out();
        graph.simulate();
        let read = Marker::new(MarkerKind::Content, "the workflow read 3.txt");
        graph.nodes[2].markers.push(read);
        let spread = Marker::new(MarkerKind::Wildcard, "a pattern matched");
        graph.nodes[0].markers.push(spread);
        let mut options = Options::default();
        options.states.insert(TaskId(2), State::Running);
        let drawing = graph.drawing(&options);
        assert_eq!(ids(&drawing), ["t1", "s1", "t9", "m1", "ms1"]);
        assert_eq!(drawing.shapes[0].state, State::Simulated);
        assert_eq!(drawing.shapes[1].state, State::Running);
        assert_eq!(
            drawing.shapes[1].lines[1],
            "7 tasks: 6 simulated, 1 running"
        );
        assert_eq!(drawing.shapes[4].kind, Kind::OpenPoint);
        assert!(
            drawing.shapes[4]
                .tooltip
                .ends_with("the workflow read 3.txt")
        );
        let last = drawing.links.last().expect("links");
        assert_eq!((last.from, last.to, last.dashed), (1, 4, true));
    }

    #[test]
    fn shortens_qualified_names() {
        let name = "NFCORE_RNASEQ:RNASEQ:FASTQ_QC:FQ_LINT (WT_REP1)";
        assert_eq!(last_parts(name, 1), "FQ_LINT (WT_REP1)");
        assert_eq!(
            last_parts("NFCORE_RNASEQ:RNASEQ:FASTQ_QC:FQ_LINT", 2),
            "FASTQ_QC:FQ_LINT"
        );
        assert_eq!(last_parts("bwa_map (sample=A)", 1), "bwa_map (sample=A)");
    }

    #[test]
    fn preserves_name_suffix() {
        assert_eq!(shorten("FASTQC (a)"), "FASTQC (a)");
        let long = "NFCORE_RNASEQ:RNASEQ:FASTQ_FASTQC_UMITOOLS_TRIMGALORE:FASTQC";
        let short = shorten(long);
        assert!(short.chars().count() <= 40, "{short}");
        assert_eq!(short, "…FASTQ_FASTQC_UMITOOLS_TRIMGALORE:FASTQC");
        let tagged = shorten("CUSTOM_GTFFILTER (genes_with_empty_tid.filtered)");
        assert_eq!(tagged, "CUSTOM_GTFFILTER (genes_with_empty_tid.…");
    }
}
