//! The stream ops that cut a stream into arrays: buffer by size or by an opening and a
//! closing item, and window, whose arrays may overlap.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::VecDeque;

use serde_json::Value as Json;
use wir::Body;
use wir::reserved::BufferRule;

use crate::callback::Callbacks;
use crate::engine::Context;
use crate::operator::{CHUNK, Operator, Pull, Step, Turn};
use crate::streams::Failure;
use crate::transforms::truth;

/// Cuts a stream into arrays.
pub enum Cutting {
    Buffer {
        rule: BufferRule,
        remainder: bool,
        callbacks: Callbacks,
        /// The array that fills now, if one is open, and the items to skip before the next.
        open: Option<Vec<Json>>,
        skipped: u32,
    },
    Window {
        size: usize,
        step: usize,
        remainder: bool,
        seen: usize,
        windows: VecDeque<Vec<Json>>,
    },
}

impl Cutting {
    pub fn buffer(rule: BufferRule, remainder: bool) -> Cutting {
        Cutting::Buffer {
            rule,
            remainder,
            callbacks: Callbacks::default(),
            open: None,
            skipped: 0,
        }
    }

    pub fn window(window: &wir::reserved::Window) -> Cutting {
        let size = window.size as usize;
        let step = window.step.map_or(size, |step| step as usize);
        Cutting::Window {
            size,
            step,
            remainder: window.remainder,
            seen: 0,
            windows: VecDeque::new(),
        }
    }

    fn take(
        &mut self,
        turn: &mut Turn<'_>,
        context: &Context<'_>,
        item: Json,
    ) -> Result<(), Failure> {
        let values = turn.values.clone();
        let holds = |callbacks: &mut Callbacks, body: &Body, item: &Json| {
            let held = callbacks
                .call(
                    context,
                    body,
                    (&["item"], std::slice::from_ref(item)),
                    &values,
                )
                .map_err(|why| turn.failed(why))?;
            truth(body, &held)
                .ok_or_else(|| turn.failed(format!("the condition gave {held}, no bool")))
        };
        match self {
            Cutting::Buffer {
                rule: BufferRule::Size(size),
                open,
                skipped,
                ..
            } => {
                if open.is_none() && *skipped < size.skip {
                    *skipped += 1;
                    return Ok(());
                }
                let buffer = open.get_or_insert_with(Vec::new);
                buffer.push(item);
                if buffer.len() == size.size as usize {
                    let full = open.take().unwrap_or_default();
                    turn.emit("out", Json::Array(full));
                    *skipped = 0;
                }
            }
            Cutting::Buffer {
                rule: BufferRule::Match(rule),
                open,
                callbacks,
                ..
            } => {
                if open.is_none() {
                    let opens = match &rule.open {
                        Some(body) => holds(callbacks, body, &item)?,
                        None => true,
                    };
                    if !opens {
                        return Ok(());
                    }
                    *open = Some(Vec::new());
                }
                let closes = holds(callbacks, &rule.close, &item)?;
                let buffer = open.get_or_insert_with(Vec::new);
                buffer.push(item);
                if closes {
                    let full = open.take().unwrap_or_default();
                    turn.emit("out", Json::Array(full));
                }
            }
            Cutting::Window {
                size,
                step,
                seen,
                windows,
                ..
            } => {
                if *seen % *step == 0 {
                    windows.push_back(Vec::new());
                }
                *seen += 1;
                for window in windows.iter_mut() {
                    window.push(item.clone());
                }
                while windows.front().is_some_and(|window| window.len() == *size) {
                    let full = windows.pop_front().unwrap_or_default();
                    turn.emit("out", Json::Array(full));
                }
            }
        }
        Ok(())
    }

    /// Gives the partial arrays at the close, when `remainder` says so.
    fn close(&mut self, turn: &mut Turn<'_>) {
        match self {
            Cutting::Buffer {
                remainder: true,
                open: Some(buffer),
                ..
            } if !buffer.is_empty() => {
                turn.emit("out", Json::Array(std::mem::take(buffer)));
            }
            Cutting::Window {
                remainder: true,
                windows,
                ..
            } => {
                for window in windows.drain(..).filter(|window| !window.is_empty()) {
                    turn.emit("out", Json::Array(window));
                }
            }
            _ => {}
        }
    }
}

impl Operator for Cutting {
    fn step(&mut self, turn: &mut Turn<'_>, context: &Context<'_>) -> Result<Step, Failure> {
        let mut moved = false;
        for _ in 0..CHUNK {
            match turn.pull("source")? {
                Pull::Item(item) => {
                    self.take(turn, context, item)?;
                    moved = true;
                }
                Pull::Pending => break,
                Pull::Closed => {
                    self.close(turn);
                    return Ok(Step::Done);
                }
            }
        }
        Ok(if moved { Step::Moved } else { Step::Idle })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, Value as Json, json};

    use crate::mixing::tests::{commit, node, steps, wired};
    use crate::operator::{Step, make};

    /// The arrays an op of `kind` gives for the items 1 to `count` of a closed stream.
    fn cut(kind: Json, count: i64) -> Vec<Json> {
        let node = node(kind, &["source"], &[], "stream");
        let mut streams = wired(&node);
        let mut operator = make(&node).unwrap();
        let items: Vec<(&str, Json)> = (1..=count).map(|item| ("source", json!(item))).collect();
        commit(&mut streams, &items, &["source"]);
        let (emitted, _, step) = steps(operator.as_mut(), &node, &mut streams, &Map::new());
        assert_eq!(step, Ok(Step::Done));
        emitted
    }

    #[test]
    fn buffer_bounds() {
        let sized = |fields: Json| cut(json!({"buffer": {"rule": {"size": fields}}}), 7);
        assert_eq!(
            sized(json!({"size": 3})),
            [json!([1, 2, 3]), json!([4, 5, 6])],
            "a partial buffer drops"
        );
        let rest = cut(
            json!({"buffer": {"rule": {"size": {"size": 3}}, "remainder": true}}),
            7,
        );
        assert_eq!(rest, [json!([1, 2, 3]), json!([4, 5, 6]), json!([7])]);
        assert_eq!(
            sized(json!({"size": 2, "skip": 1})),
            [json!([2, 3]), json!([5, 6])],
            "skip comes before each buffer"
        );
        let groovy = |code: &str| json!({"language": "groovy", "code": code});
        let matched = json!({"buffer": {"rule": {"match": {"open": groovy("{ it % 3 == 1 }"), "close": groovy("{ it % 3 == 2 }")}}}});
        assert_eq!(
            cut(matched, 7),
            [json!([1, 2]), json!([4, 5])],
            "the opening and the closing item belong to it"
        );
        let closing = json!({"buffer": {"rule": {"match": {"close": groovy("{ it == 2 }")}}, "remainder": true}});
        assert_eq!(cut(closing, 4), [json!([1, 2]), json!([3, 4])]);
    }

    #[test]
    fn window_overlap() {
        let sliding = cut(json!({"window": {"size": 3, "step": 1}}), 4);
        assert_eq!(
            sliding,
            [
                json!([1, 2, 3]),
                json!([2, 3, 4]),
                json!([3, 4]),
                json!([4])
            ],
            "partial windows at the close"
        );
        let whole = cut(
            json!({"window": {"size": 3, "step": 1, "remainder": false}}),
            4,
        );
        assert_eq!(whole, [json!([1, 2, 3]), json!([2, 3, 4])]);
        let collated = cut(json!({"window": {"size": 2}}), 5);
        assert_eq!(
            collated,
            [json!([1, 2]), json!([3, 4]), json!([5])],
            "a step of the size by default"
        );
    }
}
