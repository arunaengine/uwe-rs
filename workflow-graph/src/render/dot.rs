//! A drawing as Graphviz DOT, with the title as the graph's label, the colours of the states,
//! and the direction of the layout.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt::Write as _;

use super::{Direction, Drawing, Kind};

/// The colours of an open point: its fill and border.
const OPEN_POINT: (&str, &str) = ("#fff8e1", "#b7791f");

/// The drawing in Graphviz DOT form.
pub fn dot(drawing: &Drawing, direction: Direction) -> String {
    let mut dot = String::from("digraph workflow {\n");
    if !drawing.title.is_empty() {
        let _ = writeln!(
            dot,
            "  label=\"{}\";\n  labelloc=t;",
            escape(&drawing.title.join("\n"))
        );
    }
    let rank = match direction {
        Direction::Down => "TB",
        Direction::Right => "LR",
    };
    let _ = writeln!(dot, "  rankdir={rank};");
    dot.push_str("  node [shape=box, style=\"rounded,filled\"];\n");
    for shape in &drawing.shapes {
        let open = shape.kind == Kind::OpenPoint;
        let (fill, border) = match open {
            true => OPEN_POINT,
            false => (shape.state.fill(), shape.state.colour()),
        };
        let style = if open {
            ", style=\"rounded,dashed,filled\""
        } else {
            ""
        };
        let _ = writeln!(
            dot,
            "  {} [label=\"{}\", fillcolor=\"{fill}\", color=\"{border}\"{style}, tooltip=\"{}\"];",
            shape.id,
            escape(&shape.lines.join("\n")),
            escape(&shape.tooltip)
        );
    }
    for link in &drawing.links {
        let (Some(from), Some(to)) = (drawing.shapes.get(link.from), drawing.shapes.get(link.to))
        else {
            continue;
        };
        let mut attributes = Vec::new();
        if let Some(label) = &link.label {
            attributes.push(format!("label=\"{}\"", escape(label)));
        }
        if link.dashed {
            attributes.push("style=dashed".to_owned());
        }
        if attributes.is_empty() {
            let _ = writeln!(dot, "  {} -> {};", from.id, to.id);
        } else {
            let _ = writeln!(
                dot,
                "  {} -> {} [{}];",
                from.id,
                to.id,
                attributes.join(", ")
            );
        }
    }
    dot.push_str("}\n");
    dot
}

/// Text inside double quotes of DOT; a line end becomes `\n`.
fn escape(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}
