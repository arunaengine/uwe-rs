//! The shapes of nf-arts' pictures, and the pictures written as SVG.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::f64::consts::TAU;

/// A colour as red, green and blue.
pub(super) type Color = [u8; 3];

/// One shape of a picture, in pixels.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Shape {
    /// A stroke along a circle from one angle to another, clockwise from 12 o'clock, in turns.
    Arc {
        center: (f64, f64),
        radius: f64,
        turns: (f64, f64),
        width: f64,
        color: Color,
        opacity: f64,
    },
    /// A line through points.
    Line {
        points: Vec<(f64, f64)>,
        width: f64,
        color: Color,
        opacity: f64,
    },
    /// A filled or, when `hollow`, outlined circle.
    Dot {
        center: (f64, f64),
        radius: f64,
        hollow: bool,
        color: Color,
        opacity: f64,
    },
    /// A filled or, when `hollow`, outlined rectangle.
    Block {
        corner: (f64, f64),
        size: (f64, f64),
        hollow: bool,
        color: Color,
        opacity: f64,
    },
}

/// A picture: its size, background, shapes and caption lines.
pub(super) struct Picture {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) background: Color,
    pub(super) shapes: Vec<Shape>,
    /// The caption lines, with their colour; empty without a caption.
    pub(super) caption: Vec<String>,
    pub(super) ink: Color,
}

pub(super) fn hex(color: Color) -> String {
    format!("#{:02x}{:02x}{:02x}", color[0], color[1], color[2])
}

/// A point on a circle at a turn, clockwise from 12 o'clock.
fn at(center: (f64, f64), radius: f64, turn: f64) -> (f64, f64) {
    let angle = turn * TAU;
    (
        center.0 + radius * angle.sin(),
        center.1 - radius * angle.cos(),
    )
}

impl Picture {
    pub(super) fn svg(&self) -> String {
        let (width, height) = (self.width, self.height);
        let mut text = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" \
             viewBox=\"0 0 {width} {height}\">\n<rect width=\"100%\" height=\"100%\" fill=\"{}\"/>\n\
             <g fill=\"none\" stroke-linecap=\"round\" stroke-linejoin=\"round\">\n",
            hex(self.background)
        );
        for shape in &self.shapes {
            text.push_str(&svg_shape(shape));
            text.push('\n');
        }
        let short = f64::from(width.min(height));
        let lines = [(0.95, 0.028, "0.90"), (0.982, 0.014, "0.60")];
        for (line, (place, size, opacity)) in self.caption.iter().zip(lines) {
            text.push_str(&format!(
                "<text x=\"{:.2}\" y=\"{:.2}\" font-family=\"Helvetica, Arial, sans-serif\" \
                 font-size=\"{:.2}\" text-anchor=\"middle\" fill=\"{}\" fill-opacity=\"{opacity}\" \
                 stroke=\"none\">{}</text>\n",
                f64::from(width) / 2.0,
                f64::from(height) * place,
                short * size,
                hex(self.ink),
                escape(line)
            ));
        }
        text.push_str("</g>\n</svg>\n");
        text
    }
}

fn svg_shape(shape: &Shape) -> String {
    let paint = |color: Color, hollow: bool, width: f64, opacity: f64| match hollow {
        true => format!(
            "stroke=\"{}\" stroke-width=\"{width:.2}\" stroke-opacity=\"{opacity:.2}\"",
            hex(color)
        ),
        false => format!("fill=\"{}\" fill-opacity=\"{opacity:.2}\"", hex(color)),
    };
    match shape {
        Shape::Arc {
            center,
            radius,
            turns,
            width,
            color,
            opacity,
        } => {
            let from = at(*center, *radius, turns.0);
            let to = at(*center, *radius, turns.1);
            let large = u8::from(turns.1 - turns.0 > 0.5);
            format!(
                "<path d=\"M{:.2} {:.2} A{radius:.2} {radius:.2} 0 {large} 1 {:.2} {:.2}\" {}/>",
                from.0,
                from.1,
                to.0,
                to.1,
                paint(*color, true, *width, *opacity)
            )
        }
        Shape::Line {
            points,
            width,
            color,
            opacity,
        } => {
            let path: Vec<String> = (points.iter().enumerate())
                .map(|(place, (x, y))| {
                    format!("{}{x:.2} {y:.2}", if place == 0 { "M" } else { "L" })
                })
                .collect();
            format!(
                "<path d=\"{}\" {}/>",
                path.join(" "),
                paint(*color, true, *width, *opacity)
            )
        }
        Shape::Dot {
            center,
            radius,
            hollow,
            color,
            opacity,
        } => format!(
            "<circle cx=\"{:.2}\" cy=\"{:.2}\" r=\"{radius:.2}\" {}/>",
            center.0,
            center.1,
            paint(*color, *hollow, (radius / 4.0).max(1.0), *opacity)
        ),
        Shape::Block {
            corner,
            size,
            hollow,
            color,
            opacity,
        } => format!(
            "<rect x=\"{:.2}\" y=\"{:.2}\" width=\"{:.2}\" height=\"{:.2}\" {}/>",
            corner.0,
            corner.1,
            size.0,
            size.1,
            paint(*color, *hollow, 2.0, *opacity)
        ),
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
