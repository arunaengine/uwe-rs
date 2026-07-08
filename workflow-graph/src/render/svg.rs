//! A drawing as SVG, laid out by [`crate::layout`]: the title on top, boxes coloured by their
//! state, lines with arrows, and a legend of the states below. Labels use a monospace font,
//! so that their width is known without measuring text.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt::Write as _;

use super::{Drawing, Shape, State};
use crate::layout::{Graph, Layout, Point, Size, layout};

/// The width of a character of the monospace labels at 12 pixels: 0.6 em.
const CHARACTER: f64 = 7.2;
/// The height of a line of a label.
const LINE: f64 = 16.0;
/// The space between the text and the border of a box.
const PADDING_X: f64 = 10.0;
const PADDING_Y: f64 = 6.0;
const MARGIN: f64 = 16.0;
/// The height of a line of the title, and of the legend.
const TITLE_LINE: f64 = 20.0;
const LEGEND: f64 = 36.0;
/// An estimate of the width of a character of the title and the legend, in a proportional
/// font; generous, so that the drawing is wide enough.
const PROPORTIONAL: f64 = 7.0;

/// The drawing as an SVG document.
pub fn svg(drawing: &Drawing) -> String {
    let sizes: Vec<Size> = drawing.shapes.iter().map(size).collect();
    let edges: Vec<(usize, usize)> = drawing
        .links
        .iter()
        .map(|link| (link.from, link.to))
        .collect();
    let layout = layout(&Graph {
        sizes: sizes.clone(),
        edges,
        ..Graph::default()
    });
    let states: Vec<State> = State::ALL
        .into_iter()
        .filter(|state| drawing.shapes.iter().any(|shape| shape.state == *state))
        .collect();
    let top = if drawing.title.is_empty() {
        0.0
    } else {
        MARGIN / 2.0 + drawing.title.len() as f64 * TITLE_LINE
    };
    let legend_width: f64 = states
        .iter()
        .map(|state| 32.0 + state.name().chars().count() as f64 * PROPORTIONAL)
        .sum::<f64>()
        + 2.0 * MARGIN;
    let title_width = drawing
        .title
        .iter()
        .map(|line| line.chars().count() as f64 * PROPORTIONAL + 2.0 * MARGIN)
        .fold(0.0, f64::max);
    let width = layout.width.max(title_width).max(legend_width).ceil();
    let legend = if states.is_empty() { 0.0 } else { LEGEND };
    let height = (top + layout.height + legend).ceil();

    let mut svg = String::new();
    let _ = writeln!(
        svg,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" \
         viewBox=\"0 0 {width} {height}\" role=\"img\">"
    );
    svg.push_str(&style());
    svg.push_str(
        "<defs>\n\
         <marker id=\"arrow\" viewBox=\"0 0 10 10\" refX=\"9\" refY=\"5\" markerWidth=\"7\" \
         markerHeight=\"7\" orient=\"auto\"><path d=\"M0 0L10 5L0 10z\" fill=\"#8a94a6\"/></marker>\n\
         <marker id=\"arrow-open\" viewBox=\"0 0 10 10\" refX=\"9\" refY=\"5\" markerWidth=\"7\" \
         markerHeight=\"7\" orient=\"auto\"><path d=\"M0 0L10 5L0 10z\" fill=\"#b7791f\"/></marker>\n\
         </defs>\n",
    );
    let _ = writeln!(
        svg,
        "<rect class=\"background\" width=\"{width}\" height=\"{height}\"/>"
    );
    for (index, line) in drawing.title.iter().enumerate() {
        let class = if index == 0 { "title" } else { "note" };
        let _ = writeln!(
            svg,
            "<text class=\"{class}\" x=\"{MARGIN}\" y=\"{:.1}\">{}</text>",
            MARGIN / 2.0 + (index as f64 + 0.75) * TITLE_LINE,
            escape(line)
        );
    }
    let _ = writeln!(svg, "<g transform=\"translate(0 {top:.1})\">");
    links(&mut svg, drawing, &layout);
    shapes(&mut svg, drawing, &layout, &sizes);
    svg.push_str("</g>\n");
    legend_row(&mut svg, &states, top + layout.height);
    svg.push_str("</svg>\n");
    svg
}

/// The size of a box: its longest line and its number of lines.
fn size(shape: &Shape) -> Size {
    let longest = shape
        .lines
        .iter()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(0);
    Size {
        width: (longest as f64 * CHARACTER + 2.0 * PADDING_X).max(56.0),
        height: shape.lines.len().max(1) as f64 * LINE + 2.0 * PADDING_Y,
    }
}

fn style() -> String {
    let mut style = String::from(
        "<style>\n\
         .background { fill: #ffffff; }\n\
         text { font-family: ui-sans-serif, system-ui, -apple-system, 'Segoe UI', Helvetica, Arial, sans-serif; font-size: 12px; fill: #1a202c; }\n\
         .title { font-size: 14px; font-weight: 600; }\n\
         .note { fill: #744210; }\n\
         .shape text { font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, 'Liberation Mono', monospace; }\n\
         .shape .detail { fill: #4a5568; }\n\
         .shape rect { stroke-width: 1.2; }\n\
         .link { fill: none; stroke: #8a94a6; stroke-width: 1.2; }\n\
         .link.dashed { stroke-dasharray: 5 4; }\n\
         .link.open { stroke: #b7791f; }\n\
         .open-point rect { stroke-dasharray: 5 3; }\n\
         .open-point text { font-style: italic; fill: #744210; }\n",
    );
    for state in State::ALL {
        let (fill, border) = state.colours();
        let _ = writeln!(
            style,
            ".{} rect {{ fill: {fill}; stroke: {border}; }}",
            class(state)
        );
    }
    style.push_str("</style>\n");
    style
}

/// The CSS class of a state.
fn class(state: State) -> &'static str {
    match state {
        State::Simulated => "simulated",
        State::Succeeded => "succeeded",
        State::Failed => "failed",
        State::FromCache => "from-cache",
        State::Waiting => "waiting",
        State::OpenPoint => "open-point",
    }
}

fn links(svg: &mut String, drawing: &Drawing, layout: &Layout) {
    svg.push_str("<g class=\"links\">\n");
    for (index, link) in drawing.links.iter().enumerate() {
        let Some(points) = layout.edges.get(index).filter(|points| points.len() >= 2) else {
            continue;
        };
        let open = drawing
            .shapes
            .get(link.to)
            .is_some_and(|shape| shape.state == State::OpenPoint);
        let mut class = String::from("link");
        if link.dashed {
            class.push_str(" dashed");
        }
        if open {
            class.push_str(" open");
        }
        let arrow = if open { "arrow-open" } else { "arrow" };
        let _ = write!(
            svg,
            "<path class=\"{class}\" d=\"{}\" marker-end=\"url(#{arrow})\">",
            path(points)
        );
        if let Some(label) = &link.label {
            let _ = write!(svg, "<title>{}</title>", escape(label));
        }
        svg.push_str("</path>\n");
    }
    svg.push_str("</g>\n");
}

/// A smooth line through the points, which leaves and reaches each point vertically.
fn path(points: &[Point]) -> String {
    let mut path = format!("M{:.1} {:.1}", points[0].x, points[0].y);
    for pair in points.windows(2) {
        let (from, to) = (pair[0], pair[1]);
        let middle = (from.y + to.y) / 2.0;
        let _ = write!(
            path,
            "C{:.1} {middle:.1} {:.1} {middle:.1} {:.1} {:.1}",
            from.x, to.x, to.x, to.y
        );
    }
    path
}

fn shapes(svg: &mut String, drawing: &Drawing, layout: &Layout, sizes: &[Size]) {
    svg.push_str("<g class=\"shapes\">\n");
    for (index, shape) in drawing.shapes.iter().enumerate() {
        let (center, size) = (layout.nodes[index], sizes[index]);
        let (left, top) = (center.x - size.width / 2.0, center.y - size.height / 2.0);
        let _ = write!(
            svg,
            "<g class=\"shape {}\" id=\"{}\"><title>{}</title>\
             <rect x=\"{left:.1}\" y=\"{top:.1}\" width=\"{:.1}\" height=\"{:.1}\" rx=\"6\"/>",
            class(shape.state),
            escape(&shape.id),
            escape(&shape.tooltip),
            size.width,
            size.height
        );
        for (line_index, line) in shape.lines.iter().enumerate() {
            let y = top + PADDING_Y + (line_index as f64 + 0.75) * LINE;
            let detail = if line_index > 0 {
                " class=\"detail\""
            } else {
                ""
            };
            let _ = write!(
                svg,
                "<text x=\"{:.1}\" y=\"{y:.1}\" text-anchor=\"middle\"{detail}>{}</text>",
                center.x,
                escape(line)
            );
        }
        svg.push_str("</g>\n");
    }
    svg.push_str("</g>\n");
}

/// A row of the states that the drawing shows, with their colours.
fn legend_row(svg: &mut String, states: &[State], top: f64) {
    if states.is_empty() {
        return;
    }
    let y = top + LEGEND / 2.0;
    let mut x = MARGIN;
    svg.push_str("<g class=\"legend\">\n");
    for &state in states {
        let _ = writeln!(
            svg,
            "<g class=\"{}\"><rect x=\"{x:.1}\" y=\"{:.1}\" width=\"14\" height=\"12\" rx=\"3\"/>\
             <text x=\"{:.1}\" y=\"{:.1}\">{}</text></g>",
            class(state),
            y - 6.0,
            x + 20.0,
            y + 4.0,
            escape(state.name())
        );
        x += 32.0 + state.name().chars().count() as f64 * PROPORTIONAL;
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
    use crate::render::Link;

    fn shape(id: &str, state: State) -> Shape {
        Shape {
            id: id.to_owned(),
            lines: vec![format!("{id} <&>")],
            tooltip: "a \"tip\"".to_owned(),
            state,
        }
    }

    #[test]
    fn svg_has_the_boxes_lines_title_and_legend() {
        let drawing = Drawing {
            title: vec!["demo (nextflow): 2 tasks".to_owned()],
            shapes: vec![
                shape("t1", State::Simulated),
                shape("t2", State::Failed),
                shape("m2", State::OpenPoint),
            ],
            links: vec![
                Link {
                    from: 0,
                    to: 1,
                    dashed: false,
                    label: Some("a.txt".to_owned()),
                },
                Link {
                    from: 1,
                    to: 2,
                    dashed: true,
                    label: None,
                },
            ],
        };
        let svg = svg(&drawing);
        assert!(svg.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\""));
        assert!(svg.ends_with("</svg>\n"));
        assert!(svg.contains("id=\"t1\""));
        assert!(svg.contains("t1 &lt;&amp;&gt;"));
        assert!(svg.contains("<title>a &quot;tip&quot;</title>"));
        assert!(svg.contains("<title>a.txt</title>"));
        assert!(svg.contains("class=\"link dashed open\""));
        assert!(svg.contains(">demo (nextflow): 2 tasks</text>"));
        assert!(svg.contains(">more tasks may follow from real data</text>"));
        assert!(
            !svg.contains(">succeeded</text>"),
            "only the states it shows"
        );
        assert_eq!(
            svg,
            super::svg(&drawing),
            "the same drawing gives the same SVG"
        );
    }
}
