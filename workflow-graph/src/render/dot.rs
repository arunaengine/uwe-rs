//! A drawing as Graphviz DOT, with the title as the graph's label and the colours of the
//! states.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt::Write as _;

use super::{Drawing, State};

/// The drawing in Graphviz DOT form.
pub fn dot(drawing: &Drawing) -> String {
    let mut dot = String::from("digraph workflow {\n");
    if !drawing.title.is_empty() {
        let _ = writeln!(
            dot,
            "  label=\"{}\";\n  labelloc=t;",
            escape(&drawing.title.join("\n"))
        );
    }
    dot.push_str("  rankdir=TB;\n  node [shape=box, style=\"rounded,filled\"];\n");
    for shape in &drawing.shapes {
        let (fill, border) = shape.state.colours();
        let style = if shape.state == State::OpenPoint {
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
