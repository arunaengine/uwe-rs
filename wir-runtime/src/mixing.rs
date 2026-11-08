//! The stream ops that combine streams: mix in arrival order, concat in input order, merge
//! in rounds of one item of each stream, and zip into tuples.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::Value as Json;
use wir::Body;

use crate::callback::Callbacks;
use crate::engine::Context;
use crate::operator::{CHUNK, Operator, Pull, Step, Turn};
use crate::streams::Failure;

/// Gives the items of its stream inputs, in arrival order or one input after the other.
pub struct Mixing {
    pub inputs: Vec<String>,
    /// For concat: the input that it reads now.
    pub current: Option<usize>,
}

impl Operator for Mixing {
    fn step(&mut self, turn: &mut Turn<'_>, _context: &Context<'_>) -> Result<Step, Failure> {
        // An error of any input fails concat, also of one that it reads later; mix reads the
        // items before an error first.
        if let Some(failure) = (self.current.is_some())
            .then(|| {
                self.inputs
                    .iter()
                    .find_map(|input| turn.input_failure(input))
            })
            .flatten()
        {
            return Err(failure);
        }
        let mut moved = false;
        for _ in 0..CHUNK {
            let next = match self.current {
                Some(at) => match turn.peek(&self.inputs[at])? {
                    Pull::Item(_) => Some(at),
                    Pull::Pending => None,
                    Pull::Closed if at + 1 < self.inputs.len() => {
                        self.current = Some(at + 1);
                        moved = true;
                        continue;
                    }
                    Pull::Closed => return Ok(Step::Done),
                },
                None => {
                    let mut first: Option<(u64, usize)> = None;
                    let mut open = false;
                    for (at, input) in self.inputs.iter().enumerate() {
                        match turn.next_event(input)? {
                            Some(event) if first.is_none_or(|(least, _)| event < least) => {
                                first = Some((event, at))
                            }
                            Some(_) => {}
                            None => open |= turn.peek(input)? == Pull::Pending,
                        }
                    }
                    match (first, open) {
                        (Some((_, at)), _) => Some(at),
                        (None, true) => None,
                        (None, false) => return Ok(Step::Done),
                    }
                }
            };
            let Some(at) = next else {
                break;
            };
            if let Pull::Item(item) = turn.pull(&self.inputs[at])? {
                turn.emit("out", item);
                moved = true;
            }
        }
        Ok(if moved { Step::Moved } else { Step::Idle })
    }
}

/// Takes rounds of one item of every stream input and every value input: merge gives the
/// body's result or their items one level flat, zip gives the tuple of them.
pub struct Rounds {
    /// The inputs in order, and whether each is a stream.
    pub inputs: Vec<(String, bool)>,
    pub rule: RoundRule,
    /// A round of a value output is taken once.
    pub single: bool,
    pub callbacks: Callbacks,
}

/// What a round gives.
pub enum RoundRule {
    Merge(Option<Body>),
    Zip { remainder: bool },
}

impl Rounds {
    fn give(
        &mut self,
        turn: &Turn<'_>,
        context: &Context<'_>,
        round: Vec<Json>,
    ) -> Result<Json, Failure> {
        match &self.rule {
            RoundRule::Zip { .. } => Ok(Json::Array(round)),
            RoundRule::Merge(Some(body)) => {
                let names: Vec<&str> = self.inputs.iter().map(|(name, _)| name.as_str()).collect();
                let called = self
                    .callbacks
                    .call(context, body, (&names, &round), &turn.values);
                called.map_err(|why| turn.failed(why))
            }
            RoundRule::Merge(None) => {
                let mut items = Vec::new();
                for value in round {
                    match value {
                        Json::Array(inner) => items.extend(inner),
                        other => items.push(other),
                    }
                }
                Ok(Json::Array(items))
            }
        }
    }
}

impl Operator for Rounds {
    fn step(&mut self, turn: &mut Turn<'_>, context: &Context<'_>) -> Result<Step, Failure> {
        let streams: Vec<String> = (self.inputs.iter())
            .filter(|(_, stream)| *stream)
            .map(|(name, _)| name.clone())
            .collect();
        let remainder = matches!(self.rule, RoundRule::Zip { remainder: true });
        let mut moved = false;
        for _ in 0..CHUNK {
            // Every input is read before any is taken.
            let mut seen = Vec::new();
            for input in &streams {
                seen.push(turn.peek(input)?);
            }
            if seen.contains(&Pull::Pending) {
                break;
            }
            let full = seen.iter().all(|pull| matches!(pull, Pull::Item(_)));
            let any = seen.iter().any(|pull| matches!(pull, Pull::Item(_)));
            if !full && !(remainder && any) {
                return Ok(Step::Done);
            }
            let mut taken = Vec::new();
            for (input, pull) in streams.iter().zip(&seen) {
                taken.push(match pull {
                    Pull::Item(_) => turn
                        .take(std::slice::from_ref(input))?
                        .and_then(|mut items| items.pop())
                        .unwrap_or_default(),
                    _ => Json::Null,
                });
            }
            let mut taken = taken.into_iter();
            let round: Vec<Json> = (self.inputs.iter())
                .map(|(name, stream)| match stream {
                    true => taken.next().unwrap_or_default(),
                    false => turn.values.get(name).cloned().unwrap_or_default(),
                })
                .collect();
            let value = self.give(turn, context, round)?;
            moved = true;
            if self.single {
                turn.bind("out", value);
                return Ok(Step::Done);
            }
            turn.emit("out", value);
            if streams.is_empty() {
                return Ok(Step::Done);
            }
        }
        Ok(if moved { Step::Moved } else { Step::Idle })
    }
}

#[cfg(test)]
#[path = "mixing_tests.rs"]
pub(crate) mod tests;
