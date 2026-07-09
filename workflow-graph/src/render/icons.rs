//! The round icons of the states and of open points, as SVG symbols. Each box refers to the
//! icon of its state as `#icon-<state>`, so that the web page changes the state of a box by
//! changing one reference. The colours are those of the web page, which it can change for its
//! dark look through CSS variables of the same names.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt::Write as _;

use super::State;

/// The white mark on a filled icon.
const MARK: &str =
    "fill:none;stroke:#fff;stroke-width:2.2;stroke-linecap:round;stroke-linejoin:round";

/// The symbols of all states and of the open point, for the `<defs>` of a picture.
pub(super) fn symbols() -> String {
    let mut defs = String::new();
    for state in State::ALL {
        let _ = writeln!(
            defs,
            "<symbol id=\"icon-{}\" viewBox=\"0 0 20 20\">{}</symbol>",
            state.name(),
            icon(state)
        );
    }
    let dots: String = [6.4, 10.0, 13.6]
        .iter()
        .map(|x| format!("<circle cx=\"{x}\" cy=\"10\" r=\"1.3\" style=\"fill:#b7791f\"/>"))
        .collect();
    let _ = writeln!(
        defs,
        "<symbol id=\"icon-open\" viewBox=\"0 0 20 20\"><circle cx=\"10\" cy=\"10\" r=\"8.6\" \
         style=\"fill:#fff8e1;stroke:#b7791f;stroke-width:1.5;stroke-dasharray:2.4 1.9\"/>\
         {dots}</symbol>"
    );
    defs
}

/// A property in the colour of a state. A viewer without CSS variables keeps the first value.
fn paint(property: &str, state: State) -> String {
    let (colour, name) = (state.colour(), state.name());
    format!("{property}:{colour};{property}:var(--{name},{colour})")
}

fn icon(state: State) -> String {
    let disc = format!(
        "<circle cx=\"10\" cy=\"10\" r=\"9.5\" style=\"{}\"/>",
        paint("fill", state)
    );
    let mark = |path: &str| format!("<path d=\"{path}\" style=\"{MARK}\"/>");
    // An outlined icon, for tasks that did not run: dashed for a simulated one.
    let ring = |dashes: &str| {
        format!(
            "<circle cx=\"10\" cy=\"10\" r=\"8.6\" style=\"fill:#fff;fill:var(--card,#fff);{};\
             stroke-width:1.7{dashes}\"/>",
            paint("stroke", state)
        )
    };
    let line = |path: &str| {
        format!(
            "<path d=\"{path}\" style=\"fill:none;{};stroke-width:1.9;stroke-linecap:round;\
             stroke-linejoin:round\"/>",
            paint("stroke", state)
        )
    };
    match state {
        State::Succeeded => disc + &mark("M5.6 10.3l2.9 2.9 5.9-6.2"),
        State::Failed => disc + &mark("M6.6 6.6l6.8 6.8M13.4 6.6l-6.8 6.8"),
        // A three-quarter circle that turns, in a viewer that runs CSS animations.
        State::Running => format!(
            "<g style=\"transform-origin:10px 10px;transform-box:view-box;\
             animation:graph-turn 1.2s linear infinite\">{disc}{}</g>",
            mark("M10 4.6a5.4 5.4 0 1 1 -5.4 5.4")
        ),
        State::Stopped => {
            disc + "<rect x=\"6.5\" y=\"6.5\" width=\"7\" height=\"7\" rx=\"1.2\" \
                    style=\"fill:#fff\"/>"
        }
        // An arrow that comes back round: a result used again.
        State::Reused => {
            disc + &mark("M14.2 8.1A4.6 4.6 0 1 0 14.2 11.9")
                + "<path d=\"M15.3 9.6L16.5 13.2L12.6 11.6Z\" style=\"fill:#fff\"/>"
        }
        // A clock.
        State::Waiting => ring("") + &line("M10 5.9v4.3l2.8 1.7"),
        State::Simulated => ring(";stroke-dasharray:2.4 1.9") + &line("M6.4 10.3l2.5 2.5 4.8-5.1"),
    }
}
