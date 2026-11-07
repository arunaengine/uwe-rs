//! uwe's own drawings of a run's tasks for nf-arts: radial, mondrian, phyllotaxis and ridgeline.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::image::{Color, Shape};

/// What a picture shows of a task.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Mark {
    pub(super) process: String,
    /// Milliseconds since 1970.
    pub(super) start: f64,
    /// Milliseconds.
    pub(super) duration: f64,
    pub(super) memory: Option<f64>,
    pub(super) cached: bool,
    pub(super) failed: bool,
}

/// The marks of a run with their processes in the order of their first start.
pub(super) struct Scene<'a> {
    pub(super) marks: &'a [Mark],
    pub(super) processes: Vec<&'a str>,
    /// One colour per process.
    pub(super) colors: Vec<Color>,
    pub(super) width: f64,
    /// The height above the caption.
    pub(super) height: f64,
    /// Chooses among the ways a style may go, from the session id.
    pub(super) seed: u64,
}

impl Scene<'_> {
    fn process(&self, mark: &Mark) -> usize {
        self.processes
            .iter()
            .position(|it| *it == mark.process)
            .unwrap_or(0)
    }

    fn color(&self, mark: &Mark) -> Color {
        self.colors[self.process(mark) % self.colors.len().max(1)]
    }

    fn opacity(&self, mark: &Mark) -> f64 {
        if mark.cached { 0.3 } else { 0.8 }
    }

    /// The peak memory of a task on a log scale from 0 to 1 against the largest, 0.5 unknown.
    fn memory(&self, mark: &Mark) -> f64 {
        let largest = self
            .marks
            .iter()
            .filter_map(|it| it.memory)
            .fold(0.0, f64::max);
        match mark.memory {
            Some(memory) if largest > 0.0 => (1.0 + memory).ln() / (1.0 + largest).ln(),
            _ => 0.5,
        }
    }

    /// The first start and the time from it to the last end, at least 1 ms.
    fn span(&self) -> (f64, f64) {
        let first = self
            .marks
            .iter()
            .map(|it| it.start)
            .fold(f64::MAX, f64::min);
        let last = (self.marks.iter())
            .map(|it| it.start + it.duration)
            .fold(f64::MIN, f64::max);
        (first, (last - first).max(1.0))
    }
}

/// A ring per process, inside out in start order; each task an arc over its time, as thick as
/// its memory.
pub(super) fn radial(scene: &Scene) -> Vec<Shape> {
    let center = (scene.width / 2.0, scene.height / 2.0);
    let outer = scene.width.min(scene.height) * 0.42;
    let inner = outer * 0.25;
    let ring = (outer - inner) / scene.processes.len().max(1) as f64;
    let (first, span) = scene.span();
    let mut shapes = Vec::new();
    for mark in scene.marks {
        let from = (mark.start - first) / span;
        let to = ((mark.start + mark.duration - first) / span)
            .max(from + 0.004)
            .min(1.0);
        let mut width = ring * (0.25 + 0.6 * scene.memory(mark));
        if mark.failed {
            width *= 0.4;
        }
        shapes.push(Shape::Arc {
            center,
            radius: inner + ring * (scene.process(mark) as f64 + 0.5),
            turns: (from, to),
            width,
            color: scene.color(mark),
            opacity: scene.opacity(mark),
        });
    }
    shapes
}

/// A band per process, as wide as the time its tasks took; each task a block of the band, as
/// long as its time, as strong as its memory; failed tasks hollow. The seed turns the bands.
pub(super) fn mondrian(scene: &Scene) -> Vec<Shape> {
    let total: f64 = scene.marks.iter().map(|it| it.duration.max(1.0)).sum();
    let across = scene.seed % 2 == 0;
    let (long, short) = match across {
        true => (scene.height, scene.width),
        false => (scene.width, scene.height),
    };
    let gap = 3.0;
    let mut shapes = Vec::new();
    let mut offset = 0.0;
    for (place, process) in scene.processes.iter().enumerate() {
        let tasks: Vec<&Mark> = scene
            .marks
            .iter()
            .filter(|it| it.process == *process)
            .collect();
        let time: f64 = tasks.iter().map(|it| it.duration.max(1.0)).sum();
        let band = long * time / total;
        let mut along = 0.0;
        for mark in &tasks {
            let length = short * mark.duration.max(1.0) / time;
            let (corner, size) = match across {
                true => ((along + gap, offset + gap), (length - gap, band - gap)),
                false => ((offset + gap, along + gap), (band - gap, length - gap)),
            };
            shapes.push(Shape::Block {
                corner,
                size: (size.0.max(1.0), size.1.max(1.0)),
                hollow: mark.failed,
                color: scene.colors[place % scene.colors.len().max(1)],
                opacity: if mark.cached {
                    0.3
                } else {
                    0.45 + 0.55 * scene.memory(mark)
                },
            });
            along += length;
        }
        offset += band;
    }
    shapes
}

/// Tasks as seeds on a golden-angle spiral in start order, as large as their memory.
pub(super) fn phyllotaxis(scene: &Scene) -> Vec<Shape> {
    let center = (scene.width / 2.0, scene.height / 2.0);
    let mut marks: Vec<&Mark> = scene.marks.iter().collect();
    marks.sort_by(|one, other| one.start.total_cmp(&other.start));
    let room = scene.width.min(scene.height) * 0.45;
    let step = room / (marks.len() as f64).sqrt().max(1.0);
    let golden = std::f64::consts::PI * (3.0 - 5f64.sqrt());
    let mut shapes = Vec::new();
    for (place, mark) in marks.iter().enumerate() {
        let distance = step * (place as f64 + 0.5).sqrt();
        let angle = place as f64 * golden;
        let mut radius = step * (0.2 + 0.3 * scene.memory(mark));
        if mark.failed {
            radius *= 0.6;
        }
        shapes.push(Shape::Dot {
            center: (
                center.0 + distance * angle.cos(),
                center.1 + distance * angle.sin(),
            ),
            radius,
            hollow: mark.failed,
            color: scene.color(mark),
            opacity: scene.opacity(mark),
        });
    }
    shapes
}

/// A ridge per process, top to bottom, time left to right; its height is how many of its tasks
/// ran at a time against its busiest moment.
pub(super) fn ridgeline(scene: &Scene) -> Vec<Shape> {
    let (first, span) = scene.span();
    let rows = scene.processes.len().max(1) as f64;
    let row = scene.height * 0.9 / rows;
    let margin = scene.width * 0.05;
    let steps = 240;
    let mut shapes = Vec::new();
    for (place, process) in scene.processes.iter().enumerate() {
        let tasks: Vec<&Mark> = scene
            .marks
            .iter()
            .filter(|it| it.process == *process)
            .collect();
        let running: Vec<f64> = (0..=steps)
            .map(|step| {
                let time = first + span * step as f64 / steps as f64;
                let busy = tasks
                    .iter()
                    .filter(|it| it.start <= time && time <= it.start + it.duration);
                busy.count() as f64
            })
            .collect();
        let busiest = running.iter().copied().fold(1.0, f64::max);
        let base = scene.height * 0.05 + row * (place as f64 + 1.0);
        let points = (running.iter().enumerate())
            .map(|(step, count)| {
                let x = margin + (scene.width - 2.0 * margin) * step as f64 / steps as f64;
                (x, base - row * 1.6 * count / busiest)
            })
            .collect();
        let failed = tasks.iter().any(|it| it.failed);
        shapes.push(Shape::Line {
            points,
            width: if failed { 1.0 } else { 2.5 },
            color: scene.colors[place % scene.colors.len().max(1)],
            opacity: if tasks.iter().all(|it| it.cached) {
                0.3
            } else {
                0.85
            },
        });
    }
    shapes
}
