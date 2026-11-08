//! The stream ops that pair the items of `left` and `right` by key: join one to one and
//! first in first out, and combine every pair.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{HashMap, HashSet, VecDeque};

use serde_json::Value as Json;
use wir::Body;

use crate::callback::Callbacks;
use crate::engine::Context;
use crate::operator::{CHUNK, Operator, Pull, Step, Turn};
use crate::streams::Failure;
use crate::values;

/// The next item of two inputs in arrival order.
pub enum Arrival {
    Item(usize, Json),
    Pending,
    Closed,
}

/// Takes the earliest committed item of `inputs`; ties go to the input declared first.
pub fn arrive(turn: &mut Turn<'_>, inputs: &[&str]) -> Result<Arrival, Failure> {
    let mut first: Option<(u64, usize)> = None;
    let mut open = false;
    for (at, input) in inputs.iter().enumerate() {
        match turn.next_event(input)? {
            Some(event) if first.is_none_or(|(least, _)| event < least) => {
                first = Some((event, at))
            }
            Some(_) => {}
            None => open |= turn.peek(input)? == Pull::Pending,
        }
    }
    Ok(match (first, open) {
        (Some((_, at)), _) => match turn.pull(inputs[at])? {
            Pull::Item(item) => Arrival::Item(at, item),
            _ => Arrival::Pending,
        },
        (None, true) => Arrival::Pending,
        (None, false) => Arrival::Closed,
    })
}

/// The key of an item at the `by` positions, and its other fields; a scalar is keyed by
/// position 0 as a whole.
pub fn split_key(item: &Json, by: &[u32]) -> Result<(Vec<Json>, Vec<Json>), String> {
    let mut by: Vec<usize> = by.iter().map(|position| *position as usize).collect();
    by.sort_unstable();
    match item {
        Json::Array(fields) => {
            if let Some(position) = by.iter().find(|position| **position >= fields.len()) {
                return Err(format!(
                    "key position {position} is outside the item {item}"
                ));
            }
            let key = by
                .iter()
                .map(|position| fields[*position].clone())
                .collect();
            let rest = (fields.iter().enumerate())
                .filter(|(at, _)| !by.contains(at))
                .map(|(_, field)| field.clone())
                .collect();
            Ok((key, rest))
        }
        scalar if by == [0] => Ok((vec![scalar.clone()], Vec::new())),
        _ => Err(format!("the item {item} has no positions besides 0")),
    }
}

/// The key fields once, then the other fields of both sides; one field is the value itself.
fn assembled(key: &[Json], left: &[Json], right: &[Json]) -> Json {
    let fields: Vec<Json> = key.iter().chain(left).chain(right).cloned().collect();
    match <[Json; 1]>::try_from(fields) {
        Ok([one]) => one,
        Err(fields) => Json::Array(fields),
    }
}

/// The key fields of a key, and the other fields of the items of each side that wait for a
/// partner.
type Waiting = (Vec<Json>, [VecDeque<Vec<Json>>; 2]);

/// Joins `left` and `right` on the `by` positions.
pub struct Joining {
    pub by: Vec<u32>,
    pub remainder: bool,
    pub mismatch_error: bool,
    pub duplicate_error: bool,
    pub body: Option<Body>,
    /// The number of fields besides the key of each side, where its type fixes it.
    pub widths: [Option<usize>; 2],
    pub callbacks: Callbacks,
    waiting: HashMap<String, Waiting>,
    seen: [HashSet<String>; 2],
    order: Vec<String>,
}

impl Joining {
    pub fn new(spec: &wir::reserved::Join, widths: [Option<usize>; 2]) -> Joining {
        Joining {
            by: spec.by.clone(),
            remainder: spec.remainder,
            mismatch_error: spec.mismatch_error,
            duplicate_error: spec.duplicate_error,
            body: spec.body.clone(),
            widths,
            callbacks: Callbacks::default(),
            waiting: HashMap::new(),
            seen: Default::default(),
            order: Vec::new(),
        }
    }

    fn give(
        &mut self,
        turn: &mut Turn<'_>,
        context: &Context<'_>,
        value: Json,
    ) -> Result<(), Failure> {
        let value = match &self.body {
            Some(body) => (self
                .callbacks
                .call(context, body, (&["item"], &[value]), &turn.values))
            .map_err(|why| turn.failed(why))?,
            None => value,
        };
        turn.emit("out", value);
        Ok(())
    }

    /// Gives the unmatched items at the close, as the options say.
    fn close(&mut self, turn: &mut Turn<'_>, context: &Context<'_>) -> Result<(), Failure> {
        let unmatched = self
            .waiting
            .values()
            .any(|(_, sides)| sides.iter().any(|side| !side.is_empty()));
        if self.mismatch_error && unmatched {
            return Err(turn.failed("an item of a key has no partner"));
        }
        if !self.remainder {
            return Ok(());
        }
        for key in std::mem::take(&mut self.order) {
            let Some((fields, sides)) = self.waiting.remove(&key) else {
                continue;
            };
            for (side, queue) in sides.into_iter().enumerate() {
                for rest in queue {
                    let other = vec![Json::Null; self.widths[1 - side].unwrap_or(0)];
                    let (left, right) = if side == 0 {
                        (rest, other)
                    } else {
                        (other, rest)
                    };
                    self.give(turn, context, assembled(&fields, &left, &right))?;
                }
            }
        }
        Ok(())
    }
}

impl Operator for Joining {
    fn step(&mut self, turn: &mut Turn<'_>, context: &Context<'_>) -> Result<Step, Failure> {
        let mut moved = false;
        for _ in 0..CHUNK {
            let (side, item) = match arrive(turn, &["left", "right"])? {
                Arrival::Item(side, item) => (side, item),
                Arrival::Pending => break,
                Arrival::Closed => {
                    self.close(turn, context)?;
                    return Ok(Step::Done);
                }
            };
            moved = true;
            let (fields, rest) = split_key(&item, &self.by).map_err(|why| turn.failed(why))?;
            let key = values::canonical(&Json::Array(fields.clone()));
            if !self.seen[side].insert(key.clone()) && self.duplicate_error {
                return Err(turn.failed(format!("the key {key} comes twice")));
            }
            if !self.waiting.contains_key(&key) {
                self.order.push(key.clone());
            }
            let (_, sides) = self
                .waiting
                .entry(key)
                .or_insert_with(|| (fields.clone(), Default::default()));
            match sides[1 - side].pop_front() {
                Some(other) => {
                    let (left, right) = if side == 0 {
                        (rest, other)
                    } else {
                        (other, rest)
                    };
                    self.give(turn, context, assembled(&fields, &left, &right))?;
                }
                None => sides[side].push_back(rest),
            }
        }
        Ok(if moved { Step::Moved } else { Step::Idle })
    }
}

/// Gives every pair of a `left` and a `right` item once its second member arrives, with
/// equal keys at `by` when it is set.
pub struct Combining {
    pub by: Option<Vec<u32>>,
    pub body: Option<Body>,
    pub callbacks: Callbacks,
    pub retained: [Vec<Json>; 2],
}

impl Operator for Combining {
    fn step(&mut self, turn: &mut Turn<'_>, context: &Context<'_>) -> Result<Step, Failure> {
        let mut moved = false;
        for _ in 0..CHUNK {
            let (side, item) = match arrive(turn, &["left", "right"])? {
                Arrival::Item(side, item) => (side, item),
                Arrival::Pending => break,
                Arrival::Closed => return Ok(Step::Done),
            };
            moved = true;
            for other in self.retained[1 - side].clone() {
                let (left, right) = if side == 0 {
                    (&item, &other)
                } else {
                    (&other, &item)
                };
                let value = match &self.by {
                    None => {
                        let level = |value: &Json| match value {
                            Json::Array(items) => items.clone(),
                            other => vec![other.clone()],
                        };
                        Some(Json::Array([level(left), level(right)].concat()))
                    }
                    Some(by) => {
                        let (left_key, left_rest) =
                            split_key(left, by).map_err(|why| turn.failed(why))?;
                        let (right_key, right_rest) =
                            split_key(right, by).map_err(|why| turn.failed(why))?;
                        let same =
                            values::equal(&Json::Array(left_key.clone()), &Json::Array(right_key));
                        same.then(|| assembled(&left_key, &left_rest, &right_rest))
                    }
                };
                let Some(value) = value else {
                    continue;
                };
                let value = match &self.body {
                    Some(body) => {
                        (self
                            .callbacks
                            .call(context, body, (&["item"], &[value]), &turn.values))
                        .map_err(|why| turn.failed(why))?
                    }
                    None => value,
                };
                turn.emit("out", value);
            }
            self.retained[side].push(item);
        }
        Ok(if moved { Step::Moved } else { Step::Idle })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, Value as Json, json};

    use crate::mixing::tests::{commit, node, steps, wired};
    use crate::operator::{Step, make};
    use crate::streams::Failure;

    /// What an op of `kind` gives for left and right items committed in the given order,
    /// with both sides closed when `close` is set.
    fn paired(
        kind: Json,
        items: &[(&str, Json)],
        close: bool,
        tuples: Option<usize>,
    ) -> (Vec<Json>, Result<Step, Failure>) {
        let mut node = node(kind, &["left", "right"], &[], "stream");
        if let Some(width) = tuples {
            for port in &mut node.op.inputs {
                port.r#type = wir::Type::Tuple(vec![wir::Type::Dynamic; width]);
            }
        }
        let mut streams = wired(&node);
        let mut operator = make(&node).unwrap();
        for item in items {
            commit(&mut streams, std::slice::from_ref(item), &[]);
        }
        if close {
            commit(&mut streams, &[], &["left", "right"]);
        }
        let (emitted, _, step) = steps(operator.as_mut(), &node, &mut streams, &Map::new());
        (emitted, step)
    }

    #[test]
    fn join_fifo() {
        let items = [
            ("left", json!(["a", 1])),
            ("right", json!(["a", 10])),
            ("left", json!(["a", 2])),
            ("right", json!(["a", 20])),
            ("right", json!(["b", 5])),
        ];
        let (emitted, step) = paired(json!({"join": {}}), &items, true, None);
        assert_eq!(
            emitted,
            [json!(["a", 1, 10]), json!(["a", 2, 20])],
            "one to one, not every pair"
        );
        assert_eq!(step, Ok(Step::Done), "an unmatched item is dropped");
        let scalars = [("left", json!("a")), ("right", json!("a"))];
        assert_eq!(
            paired(json!({"join": {}}), &scalars, true, None).0,
            [json!("a")],
            "one field is the value"
        );
    }

    #[test]
    fn join_failures() {
        let twice = [
            ("left", json!(["a", 1])),
            ("right", json!(["a", 9])),
            ("left", json!(["a", 2])),
        ];
        let (emitted, step) = paired(
            json!({"join": {"duplicate_error": true}}),
            &twice,
            false,
            None,
        );
        assert_eq!(emitted, [json!(["a", 1, 9])]);
        assert!(
            step.unwrap_err().message.contains("comes twice"),
            "also after the first key matched"
        );
        let alone = [("left", json!(["a", 1]))];
        let (_, step) = paired(
            json!({"join": {"mismatch_error": true}}),
            &alone,
            true,
            None,
        );
        assert!(step.unwrap_err().message.contains("no partner"));
        let outside = [("left", json!(["a"]))];
        let (_, step) = paired(json!({"join": {"by": [1]}}), &outside, false, None);
        assert!(step.unwrap_err().message.contains("outside the item"));
    }

    #[test]
    fn join_remainder() {
        let items = [
            ("left", json!(["b", 1])),
            ("left", json!(["a", 2])),
            ("right", json!(["c", 3])),
            ("right", json!(["a", 9])),
        ];
        let (emitted, step) = paired(json!({"join": {"remainder": true}}), &items, true, Some(2));
        assert_eq!(step, Ok(Step::Done));
        assert_eq!(
            emitted,
            [
                json!(["a", 2, 9]),
                json!(["b", 1, null]),
                json!(["c", null, 3])
            ],
            "first-seen key order"
        );
    }

    #[test]
    fn combine_pairs() {
        let items = [
            ("left", json!(1)),
            ("right", json!("x")),
            ("left", json!(2)),
            ("right", json!("y")),
        ];
        let (emitted, _) = paired(json!({"combine": {}}), &items, true, None);
        let pairs = [
            json!([1, "x"]),
            json!([2, "x"]),
            json!([1, "y"]),
            json!([2, "y"]),
        ];
        assert_eq!(
            emitted, pairs,
            "every pair once, when its second member arrives"
        );
        let keyed = [
            ("left", json!(["a", 1])),
            ("left", json!(["b", 2])),
            ("right", json!(["a", "x"])),
            ("right", json!(["a", "z"])),
        ];
        let (emitted, _) = paired(json!({"combine": {"by": [0]}}), &keyed, true, None);
        assert_eq!(emitted, [json!(["a", 1, "x"]), json!(["a", 1, "z"])]);
    }
}
