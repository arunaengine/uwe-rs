//! A drawing as SVG, laid out by [`crate::layout`] down or to the right. Each task is a rounded
//! box with the round icon of its state and its name; each step, or step folded into one box,
//! is a box with its name and how many tasks it has in which states, stacked when it has
//! several; each open point of a simulated graph is a dashed box beside the task or step before
//! it. Smooth lines join the boxes. The title is on top and a legend of the states below.
//!
//! The web page of runs shows the same picture and makes it move, zoom and follow a run: it
//! reads the classes and `data-` attributes of the boxes and turns the icons of `#icon-<state>`.
//! Names use a monospace font, so that their width is known without measuring text.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt::Write as _;

use super::{Direction, Drawing, Kind, Shape, State, icons};
use crate::MORE_TASKS;
use crate::layout::{Graph, Layout, Point, Size, layout};

/// The width of a character of the monospace names at 12 pixels, and of the details at 11.
const CHARACTER: f64 = 7.2;
const SMALL: f64 = 6.6;
/// The heights of the boxes of a task, of a step and of an open point.
const TASK_HEIGHT: f64 = 30.0;
const STEP_HEIGHT: f64 = 42.0;
const OPEN_HEIGHT: f64 = 26.0;
/// The size of an icon, and of the icon of an open point.
const ICON: f64 = 18.0;
const OPEN_ICON: f64 = 16.0;
/// The space before the icon, between the icon and the text, and after the text.
const BEFORE_ICON: f64 = 7.0;
const AFTER_ICON: f64 = 8.0;
const AFTER_TEXT: f64 = 12.0;
/// How far the second box of a step with several tasks lies behind the first.
const STACK: f64 = 4.0;
const MARGIN: f64 = 16.0;
/// The height of a line of the title, and of the legend.
const TITLE_LINE: f64 = 20.0;
const LEGEND: f64 = 40.0;
/// An estimate of the width of a character of the title and the legend, in a proportional
/// font; generous, so that the drawing is wide enough.
const PROPORTIONAL: f64 = 7.0;

/// The drawing as an SVG document.
pub fn svg(drawing: &Drawing, direction: Direction) -> String {
    let frames: Vec<Frame> = drawing.shapes.iter().map(Frame::of).collect();
    let layout = layout(&Graph {
        sizes: frames.iter().map(Frame::size).collect(),
        edges: (drawing.links.iter())
            .map(|link| (link.from, link.to))
            .collect(),
        // Each open point sits beside the box before it.
        beside: (drawing.links.iter())
            .filter(|link| is_open(drawing, link.to))
            .map(|link| (link.to, link.from))
            .collect(),
        direction,
    });
    let states: Vec<State> = State::ALL
        .into_iter()
        .filter(|state| {
            let shown = |shape: &Shape| shape.kind != Kind::OpenPoint && shape.state == *state;
            drawing.shapes.iter().any(shown)
        })
        .collect();
    let open = drawing
        .shapes
        .iter()
        .any(|shape| shape.kind == Kind::OpenPoint);
    let top = if drawing.title.is_empty() {
        0.0
    } else {
        MARGIN / 2.0 + drawing.title.len() as f64 * TITLE_LINE
    };
    let title_width = drawing
        .title
        .iter()
        .map(|line| line.chars().count() as f64 * PROPORTIONAL + 2.0 * MARGIN)
        .fold(0.0, f64::max);
    let entries = legend_entries(&states, open);
    let legend_width: f64 = entries.iter().map(|(_, text)| entry_width(text)).sum();
    let width = (layout.width)
        .max(title_width)
        .max(legend_width + 2.0 * MARGIN)
        .ceil();
    let legend = if entries.is_empty() { 0.0 } else { LEGEND };
    let height = (top + layout.height + legend).ceil();

    let mut svg = String::new();
    let _ = writeln!(
        svg,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" \
         viewBox=\"0 0 {width} {height}\" class=\"graph-picture\" data-direction=\"{}\" \
         role=\"img\">",
        direction.name()
    );
    svg.push_str(STYLE);
    svg.push_str(
        "<defs>\n<marker id=\"arrow\" viewBox=\"0 0 10 10\" refX=\"9\" refY=\"5\" \
         markerWidth=\"7\" markerHeight=\"7\" orient=\"auto\"><path class=\"arrow\" \
         d=\"M0 0L10 5L0 10z\"/></marker>\n",
    );
    svg.push_str(&icons::symbols());
    svg.push_str("</defs>\n");
    let _ = writeln!(
        svg,
        "<rect class=\"background\" width=\"{width}\" height=\"{height}\"/>"
    );
    heading(&mut svg, &drawing.title);
    let _ = writeln!(
        svg,
        "<g class=\"graph\" transform=\"translate(0 {top:.1})\" data-width=\"{:.1}\" \
         data-height=\"{:.1}\">",
        layout.width, layout.height
    );
    edges(&mut svg, drawing, &layout, direction);
    nodes(&mut svg, drawing, &layout, &frames);
    svg.push_str("</g>\n");
    legend_row(&mut svg, &entries, top + layout.height);
    svg.push_str("</svg>\n");
    svg
}

/// The look of the picture. Every rule starts with the class of the picture, so that it
/// changes nothing else on the web page that shows it. A viewer without CSS variables keeps
/// the first value of a property.
const STYLE: &str = "<style>
.graph-picture text { font-family: ui-sans-serif, system-ui, -apple-system, 'Segoe UI', Helvetica, Arial, sans-serif; font-size: 12px; fill: #1d2126; fill: var(--text, #1d2126); }
.graph-picture .background { fill: #ffffff; }
.graph-picture .title { font-size: 14px; font-weight: 600; }
.graph-picture .note { fill: #7a5a12; }
.graph-picture .node text { font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, 'Liberation Mono', monospace; }
.graph-picture .node .detail { font-size: 11px; fill: #5c6570; fill: var(--quiet, #5c6570); }
.graph-picture .step .label { font-weight: 600; }
.graph-picture .box, .graph-picture .stack { fill: #ffffff; fill: var(--card, #ffffff); stroke: #c3cad3; stroke: var(--node-line, #c3cad3); stroke-width: 1; }
.graph-picture .failed > .box { stroke: #c0392b; stroke: var(--failed, #c0392b); }
.graph-picture .running > .box { stroke: #1f6fd1; stroke: var(--running, #1f6fd1); }
.graph-picture .open-point .box { fill: #fff8e1; stroke: #b7791f; stroke-dasharray: 4 3; }
.graph-picture .open-point text { font-size: 11px; font-style: italic; fill: #7a5a12; }
.graph-picture .edge { fill: none; stroke: #9aa4b2; stroke: var(--edge, #9aa4b2); stroke-width: 1.3; }
.graph-picture .edge.dashed { stroke-dasharray: 5 4; }
.graph-picture .edge.bundle { stroke-width: 3; }
.graph-picture .edge.open { stroke: #b7791f; stroke-dasharray: 3 3; }
.graph-picture .arrow { fill: #9aa4b2; fill: var(--edge, #9aa4b2); }
@keyframes graph-turn { to { transform: rotate(360deg); } }
</style>
";

/// The box of a shape, and whether a second box lies behind it.
struct Frame {
    width: f64,
    height: f64,
    stacked: bool,
}

impl Frame {
    fn of(shape: &Shape) -> Frame {
        let text = |index: usize, character: f64| {
            let line = shape.lines.get(index);
            line.map_or(0.0, |line| line.chars().count() as f64 * character)
        };
        match &shape.kind {
            Kind::Task(_) => Frame {
                width: BEFORE_ICON + ICON + AFTER_ICON + text(0, CHARACTER) + AFTER_TEXT,
                height: TASK_HEIGHT,
                stacked: false,
            },
            Kind::Step { tasks, .. } => {
                // A step that goes on keeps room for its counts as they change.
                let mut detail = text(1, SMALL);
                if matches!(shape.state, State::Running | State::Waiting) {
                    let room = format!("{tasks} tasks: {tasks} succeeded, {tasks} running");
                    detail = detail.max(room.chars().count() as f64 * SMALL);
                }
                Frame {
                    width: BEFORE_ICON
                        + ICON
                        + AFTER_ICON
                        + text(0, CHARACTER).max(detail)
                        + AFTER_TEXT,
                    height: STEP_HEIGHT,
                    stacked: *tasks > 1,
                }
            }
            Kind::OpenPoint => Frame {
                width: BEFORE_ICON + OPEN_ICON + AFTER_ICON + text(0, SMALL) + AFTER_TEXT,
                height: OPEN_HEIGHT,
                stacked: false,
            },
        }
    }

    /// The room that the layout gives the box, with the box behind it.
    fn size(&self) -> Size {
        let behind = if self.stacked { STACK } else { 0.0 };
        Size {
            width: self.width + behind,
            height: self.height + behind,
        }
    }
}

fn is_open(drawing: &Drawing, shape: usize) -> bool {
    drawing
        .shapes
        .get(shape)
        .is_some_and(|shape| shape.kind == Kind::OpenPoint)
}

fn heading(svg: &mut String, title: &[String]) {
    if title.is_empty() {
        return;
    }
    svg.push_str("<g class=\"heading\">\n");
    for (index, line) in title.iter().enumerate() {
        let class = if index == 0 { "title" } else { "note" };
        let _ = writeln!(
            svg,
            "<text class=\"{class}\" x=\"{MARGIN}\" y=\"{:.1}\">{}</text>",
            MARGIN / 2.0 + (index as f64 + 0.75) * TITLE_LINE,
            escape(line)
        );
    }
    svg.push_str("</g>\n");
}

fn edges(svg: &mut String, drawing: &Drawing, layout: &Layout, direction: Direction) {
    svg.push_str("<g class=\"edges\">\n");
    for (index, link) in drawing.links.iter().enumerate() {
        let Some(points) = layout.edges.get(index).filter(|points| points.len() >= 2) else {
            continue;
        };
        // A line under a bundle is only there for the layout.
        if link.hidden {
            continue;
        }
        let open = is_open(drawing, link.to);
        let class = match (open, link.dashed, link.bundled > 0) {
            (true, _, _) => "edge open",
            (false, true, false) => "edge dashed",
            (false, true, true) => "edge dashed bundle",
            (false, false, false) => "edge",
            (false, false, true) => "edge bundle",
        };
        let arrow = if open {
            ""
        } else {
            " marker-end=\"url(#arrow)\""
        };
        // The ids of the boxes it joins, so that the web page can mark a chain of tasks.
        let id = |index: usize| {
            drawing
                .shapes
                .get(index)
                .map_or("", |shape| shape.id.as_str())
        };
        let _ = write!(
            svg,
            "<path class=\"{class}\" data-from=\"{}\" data-to=\"{}\" d=\"{}\"{arrow}>",
            escape(id(link.from)),
            escape(id(link.to)),
            path(points, direction)
        );
        if let Some(label) = &link.label {
            let _ = write!(svg, "<title>{}</title>", escape(label));
        }
        svg.push_str("</path>\n");
    }
    svg.push_str("</g>\n");
}

/// A smooth line through the points, which leaves and reaches each point along the direction
/// of the layout: down, or to the right.
fn path(points: &[Point], direction: Direction) -> String {
    let mut path = format!("M{:.1} {:.1}", points[0].x, points[0].y);
    for pair in points.windows(2) {
        let (from, to) = (pair[0], pair[1]);
        let _ = match direction {
            Direction::Down => {
                let middle = (from.y + to.y) / 2.0;
                write!(
                    path,
                    "C{:.1} {middle:.1} {:.1} {middle:.1} {:.1} {:.1}",
                    from.x, to.x, to.x, to.y
                )
            }
            Direction::Right => {
                let middle = (from.x + to.x) / 2.0;
                write!(
                    path,
                    "C{middle:.1} {:.1} {middle:.1} {:.1} {:.1} {:.1}",
                    from.y, to.y, to.x, to.y
                )
            }
        };
    }
    path
}

fn nodes(svg: &mut String, drawing: &Drawing, layout: &Layout, frames: &[Frame]) {
    svg.push_str("<g class=\"nodes\">\n");
    // The box that each open point follows.
    let mut hosts = vec![None; drawing.shapes.len()];
    for link in &drawing.links {
        if is_open(drawing, link.to) {
            hosts[link.to] = drawing.shapes.get(link.from).map(|shape| shape.id.as_str());
        }
    }
    for (index, shape) in drawing.shapes.iter().enumerate() {
        node(
            svg,
            shape,
            &frames[index],
            layout.nodes[index],
            hosts[index],
        );
    }
    svg.push_str("</g>\n");
}

/// A box, drawn around the center that the layout gave it.
fn node(svg: &mut String, shape: &Shape, frame: &Frame, center: Point, host: Option<&str>) {
    let (kind, data, icon, icon_size, radius) = match &shape.kind {
        Kind::Task(id) => (
            "task",
            format!(" data-task=\"{id}\""),
            shape.state.name(),
            ICON,
            TASK_HEIGHT / 2.0,
        ),
        Kind::Step { name, tasks } => (
            "step",
            format!(" data-step=\"{}\" data-tasks=\"{tasks}\"", escape(name)),
            shape.state.name(),
            ICON,
            8.0,
        ),
        Kind::OpenPoint => (
            "open-point",
            host.map(|host| format!(" data-host=\"{}\"", escape(host)))
                .unwrap_or_default(),
            "open",
            OPEN_ICON,
            OPEN_HEIGHT / 2.0,
        ),
    };
    let state = match shape.kind {
        Kind::OpenPoint => String::new(),
        _ => format!(" {}", shape.state.name()),
    };
    let size = frame.size();
    let (left, top) = (-size.width / 2.0, -size.height / 2.0);
    let _ = write!(
        svg,
        "<g class=\"node {kind}{state}\" id=\"{}\"{data} transform=\"translate({:.1} {:.1})\">\
         <title>{}</title>",
        escape(&shape.id),
        center.x,
        center.y,
        escape(&shape.tooltip)
    );
    let rectangle = |class: &str, x: f64, y: f64| {
        format!(
            "<rect class=\"{class}\" x=\"{x:.1}\" y=\"{y:.1}\" width=\"{:.1}\" height=\"{:.1}\" \
             rx=\"{radius:.1}\"/>",
            frame.width, frame.height
        )
    };
    if frame.stacked {
        svg.push_str(&rectangle("stack", left + STACK, top + STACK));
    }
    svg.push_str(&rectangle("box", left, top));
    let middle = top + frame.height / 2.0;
    let icon_x = left + BEFORE_ICON;
    let _ = write!(
        svg,
        "<use class=\"icon\" href=\"#icon-{icon}\" x=\"{icon_x:.1}\" y=\"{:.1}\" \
         width=\"{icon_size}\" height=\"{icon_size}\"/>",
        middle - icon_size / 2.0
    );
    let text_x = icon_x + icon_size + AFTER_ICON;
    match shape.lines.as_slice() {
        [name, detail, ..] => {
            let _ = write!(
                svg,
                "<text class=\"label\" x=\"{text_x:.1}\" y=\"{:.1}\">{}</text>\
                 <text class=\"detail\" x=\"{text_x:.1}\" y=\"{:.1}\">{}</text>",
                middle - 3.5,
                escape(name),
                middle + 11.5,
                escape(detail)
            );
        }
        [name] => {
            let _ = write!(
                svg,
                "<text class=\"label\" x=\"{text_x:.1}\" y=\"{:.1}\">{}</text>",
                middle + 4.0,
                escape(name)
            );
        }
        [] => {}
    }
    svg.push_str("</g>\n");
}

/// The icon and text of each entry of the legend: the states that the drawing shows, and the
/// open point.
fn legend_entries(states: &[State], open: bool) -> Vec<(&'static str, &'static str)> {
    let mut entries: Vec<(&str, &str)> = (states.iter())
        .map(|state| (state.name(), state.name()))
        .collect();
    if open {
        entries.push(("open", MORE_TASKS));
    }
    entries
}

fn entry_width(text: &str) -> f64 {
    14.0 + 6.0 + text.chars().count() as f64 * PROPORTIONAL + 18.0
}

fn legend_row(svg: &mut String, entries: &[(&str, &str)], top: f64) {
    if entries.is_empty() {
        return;
    }
    let y = top + LEGEND / 2.0;
    let mut x = MARGIN;
    svg.push_str("<g class=\"legend\">\n");
    for (icon, text) in entries {
        let _ = writeln!(
            svg,
            "<use href=\"#icon-{icon}\" x=\"{x:.1}\" y=\"{:.1}\" width=\"14\" height=\"14\"/>\
             <text x=\"{:.1}\" y=\"{:.1}\">{}</text>",
            y - 7.0,
            x + 20.0,
            y + 4.0,
            escape(text)
        );
        x += entry_width(text);
    }
    svg.push_str("</g>\n");
}

/// Text inside an XML element or attribute.
fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            other => escaped.push(other),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TaskId;
    use crate::render::Link;

    fn task(id: u64, state: State) -> Shape {
        Shape {
            id: format!("t{id}"),
            kind: Kind::Task(TaskId(id)),
            lines: vec![format!("t{id} <&>")],
            tooltip: "a \"tip\"".to_owned(),
            state,
        }
    }

    fn drawing() -> Drawing {
        Drawing {
            title: vec!["demo (nextflow): 9 tasks".to_owned()],
            shapes: vec![
                task(1, State::Simulated),
                Shape {
                    id: "s1".to_owned(),
                    kind: Kind::Step {
                        name: "FASTQC".to_owned(),
                        tasks: 7,
                    },
                    lines: vec!["FASTQC".to_owned(), "7 tasks".to_owned()],
                    tooltip: "FASTQC\n7 tasks: 7 failed".to_owned(),
                    state: State::Failed,
                },
                Shape {
                    id: "ms1".to_owned(),
                    kind: Kind::OpenPoint,
                    lines: vec!["more tasks may follow".to_owned()],
                    tooltip: MORE_TASKS.to_owned(),
                    state: State::default(),
                },
            ],
            links: vec![
                Link {
                    from: 0,
                    to: 1,
                    dashed: false,
                    label: Some("a.txt".to_owned()),
                    ..Link::default()
                },
                Link {
                    from: 1,
                    to: 2,
                    dashed: true,
                    ..Link::default()
                },
            ],
        }
    }

    #[test]
    fn svg_has_the_boxes_lines_title_and_legend() {
        let drawing = drawing();
        let svg = svg(&drawing, Direction::Down);
        assert!(svg.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\""));
        assert!(svg.ends_with("</svg>\n"));
        assert!(svg.contains("class=\"node task simulated\" id=\"t1\" data-task=\"1\""));
        assert!(svg.contains("href=\"#icon-simulated\""));
        assert!(svg.contains("t1 &lt;&amp;&gt;"));
        assert!(svg.contains("<title>a &quot;tip&quot;</title>"));
        assert!(svg.contains("<title>a.txt</title>"));
        assert!(svg.contains("class=\"node step failed\" id=\"s1\" data-step=\"FASTQC\""));
        assert!(
            svg.contains("class=\"stack\""),
            "a step of 7 tasks is stacked"
        );
        assert!(svg.contains(">7 tasks</text>"));
        assert!(svg.contains("class=\"node open-point\" id=\"ms1\" data-host=\"s1\""));
        assert!(svg.contains("class=\"edge open\""));
        assert!(svg.contains("class=\"edge\" data-from=\"t1\" data-to=\"s1\""));
        assert!(svg.contains(">demo (nextflow): 9 tasks</text>"));
        assert!(
            svg.contains(&format!(">{MORE_TASKS}</text>")),
            "in the legend"
        );
        assert!(
            svg.contains("<symbol id=\"icon-running\""),
            "every icon is there"
        );
        assert!(
            !svg.contains(">succeeded</text>"),
            "the legend names only the states it shows"
        );
        assert_eq!(svg, super::svg(&drawing, Direction::Down), "the same SVG");
    }

    /// The center of the box with this id.
    fn center(svg: &str, id: &str) -> Point {
        let start = svg.find(&format!("id=\"{id}\"")).expect("the box");
        let rest = &svg[start..];
        let translate = &rest[rest.find("translate(").expect("a place") + 10..];
        let numbers: Vec<f64> = translate[..translate.find(')').expect("an end")]
            .split(' ')
            .map(|number| number.parse().expect("a number"))
            .collect();
        Point {
            x: numbers[0],
            y: numbers[1],
        }
    }

    #[test]
    fn open_points_sit_beside_their_box_in_either_direction() {
        let drawing = drawing();
        let down = svg(&drawing, Direction::Down);
        let (step, open) = (center(&down, "s1"), center(&down, "ms1"));
        assert_eq!(step.y, open.y);
        assert!(open.x > step.x);
        assert!(down.contains("data-direction=\"down\""));
        let right = svg(&drawing, Direction::Right);
        let (first, step, open) = (
            center(&right, "t1"),
            center(&right, "s1"),
            center(&right, "ms1"),
        );
        assert!(step.x > first.x, "the step follows the task to the right");
        assert_eq!(step.x, open.x);
        assert!(open.y > step.y);
    }
}
