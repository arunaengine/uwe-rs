//! The stream ops that select items: take, first, last, unique, distinct and until, and
//! if_empty, which fills an empty input.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashSet;

use serde_json::Value as Json;
use wir::Body;
use wir::reserved::EmptyRule;

use crate::callback::Callbacks;
use crate::engine::Context;
use crate::operator::{CHUNK, Operator, Pull, Step, Turn};
use crate::streams::Failure;
use crate::transforms::truth;
use crate::values;

/// What a selecting op keeps.
pub enum Choice {
    /// The first items, as many as are left; a negative count keeps all.
    Take(i64),
    First(Option<Body>, EmptyRule),
    Last(Option<Body>, EmptyRule, Option<Json>),
    Unique(Option<Body>, HashSet<String>),
    /// The key of the item before, if there is one.
    Distinct(Option<Body>, Option<String>),
    Until(Body, bool),
    /// The value or body that fills an empty input, and whether an item came.
    IfEmpty(Option<Json>, Option<Body>, bool),
}

/// Selects items of `source`; a value `source` is selected once.
pub struct Selecting {
    pub choice: Choice,
    pub stream: bool,
    pub callbacks: Callbacks,
}

impl Selecting {
    pub fn new(choice: Choice, stream: bool) -> Selecting {
        Selecting {
            choice,
            stream,
            callbacks: Callbacks::default(),
        }
    }

    fn call(
        &mut self,
        turn: &Turn<'_>,
        context: &Context<'_>,
        body: &Body,
        item: &Json,
    ) -> Result<Json, Failure> {
        let arguments = std::slice::from_ref(item);
        (self
            .callbacks
            .call(context, body, (&["item"], arguments), &turn.values))
        .map_err(|why| turn.failed(why))
    }

    fn holds(
        &mut self,
        turn: &Turn<'_>,
        context: &Context<'_>,
        body: &Option<Body>,
        item: &Json,
    ) -> Result<bool, Failure> {
        let Some(body) = body else {
            return Ok(true);
        };
        let held = self.call(turn, context, body, item)?;
        truth(body, &held).ok_or_else(|| turn.failed(format!("the condition gave {held}, no bool")))
    }

    fn key(
        &mut self,
        turn: &Turn<'_>,
        context: &Context<'_>,
        body: &Option<Body>,
        item: &Json,
    ) -> Result<String, Failure> {
        let key = match body {
            Some(body) => self.call(turn, context, body, item)?,
            None => item.clone(),
        };
        Ok(values::canonical(&key))
    }

    /// Takes one item; whether the op ends with it.
    fn take(
        &mut self,
        turn: &mut Turn<'_>,
        context: &Context<'_>,
        item: Json,
    ) -> Result<bool, Failure> {
        match &mut self.choice {
            Choice::Take(left) => {
                turn.emit("out", item);
                *left -= 1;
                Ok(*left == 0)
            }
            Choice::First(predicate, _) => {
                let predicate = predicate.clone();
                let found = self.holds(turn, context, &predicate, &item)?;
                if found {
                    turn.bind("out", item);
                }
                Ok(found)
            }
            Choice::Last(predicate, ..) => {
                let predicate = predicate.clone();
                if self.holds(turn, context, &predicate, &item)?
                    && let Choice::Last(_, _, kept) = &mut self.choice
                {
                    *kept = Some(item);
                }
                Ok(false)
            }
            Choice::Unique(key, _) => {
                let key = key.clone();
                let key = self.key(turn, context, &key, &item)?;
                if let Choice::Unique(_, seen) = &mut self.choice
                    && seen.insert(key)
                {
                    turn.emit("out", item);
                }
                Ok(false)
            }
            Choice::Distinct(key, _) => {
                let key = key.clone();
                let key = self.key(turn, context, &key, &item)?;
                if let Choice::Distinct(_, previous) = &mut self.choice {
                    if previous.as_ref() != Some(&key) {
                        turn.emit("out", item);
                    }
                    *previous = Some(key);
                }
                Ok(false)
            }
            Choice::Until(predicate, inclusive) => {
                let (predicate, inclusive) = (Some(predicate.clone()), *inclusive);
                let stop = self.holds(turn, context, &predicate, &item)?;
                if !stop || inclusive {
                    turn.emit("out", item);
                }
                Ok(stop)
            }
            Choice::IfEmpty(_, _, seen) => {
                *seen = true;
                turn.emit("out", item);
                Ok(false)
            }
        }
    }

    /// Binds or gives what the op has at the close of its input.
    fn close(&mut self, turn: &mut Turn<'_>, context: &Context<'_>) -> Result<(), Failure> {
        let empty_value = |rule: &EmptyRule, turn: &mut Turn<'_>, default: Option<Json>| match rule
        {
            EmptyRule::Default => {
                match default {
                    Some(value) => turn.bind("out", value),
                    None => turn.close_empty("out"),
                }
                Ok(())
            }
            EmptyRule::Close => {
                turn.close_empty("out");
                Ok(())
            }
            EmptyRule::Null => {
                turn.bind("out", Json::Null);
                Ok(())
            }
            EmptyRule::Value(value) => {
                turn.bind("out", value.clone());
                Ok(())
            }
            EmptyRule::Error => Err(turn.failed("no item matched")),
        };
        match &self.choice {
            Choice::First(_, rule) => empty_value(rule, turn, None),
            Choice::Last(_, _, Some(kept)) => {
                turn.bind("out", kept.clone());
                Ok(())
            }
            Choice::Last(_, rule, None) => empty_value(rule, turn, Some(Json::Null)),
            Choice::IfEmpty(value, body, false) => {
                let fill = match (value, body) {
                    (Some(value), _) => value.clone(),
                    (None, Some(body)) => {
                        let body = body.clone();
                        (self
                            .callbacks
                            .call(context, &body, (&[], &[]), &turn.values))
                        .map_err(|why| turn.failed(why))?
                    }
                    (None, None) => {
                        return Err(turn.failed("an if_empty without a value or a body"));
                    }
                };
                match self.stream {
                    true => turn.emit("out", fill),
                    false => turn.bind("out", fill),
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

impl Operator for Selecting {
    fn step(&mut self, turn: &mut Turn<'_>, context: &Context<'_>) -> Result<Step, Failure> {
        if !self.stream {
            // The value form of if_empty forwards a value, null included, and fills an empty one.
            match turn.values.get("source").cloned() {
                Some(value) => turn.bind("out", value),
                None => self.close(turn, context)?,
            }
            return Ok(Step::Done);
        }
        if matches!(self.choice, Choice::Take(0)) {
            turn.detach("source");
            return Ok(Step::Done);
        }
        let mut moved = false;
        for _ in 0..CHUNK {
            match turn.pull("source")? {
                Pull::Item(item) => {
                    moved = true;
                    if self.take(turn, context, item)? {
                        turn.detach("source");
                        return Ok(Step::Done);
                    }
                }
                Pull::Pending => break,
                Pull::Closed => {
                    self.close(turn, context)?;
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

    /// What an op of `kind` gives and binds for `items`, closed when `close` is set.
    fn selected(
        kind: Json,
        out: &str,
        items: &[Json],
        close: bool,
    ) -> (Vec<Json>, Option<Json>, Step) {
        let node = node(kind, &["source"], &[], out);
        let mut streams = wired(&node);
        let mut operator = make(&node).unwrap();
        let items: Vec<(&str, Json)> = items.iter().map(|item| ("source", item.clone())).collect();
        commit(&mut streams, &items, if close { &["source"] } else { &[] });
        let (emitted, bound, step) = steps(operator.as_mut(), &node, &mut streams, &Map::new());
        (emitted, bound, step.unwrap())
    }

    fn numbers(count: i64) -> Vec<Json> {
        (1..=count).map(Json::from).collect()
    }

    fn groovy(code: &str) -> Json {
        json!({"language": "groovy", "code": code})
    }

    #[test]
    fn take_detaches() {
        assert_eq!(
            selected(json!({"take": {"count": 2}}), "stream", &numbers(5), false),
            (numbers(2), None, Step::Done)
        );
        assert_eq!(
            selected(json!({"take": {"count": 0}}), "stream", &numbers(5), false),
            (vec![], None, Step::Done)
        );
        assert_eq!(
            selected(json!({"take": {"count": -1}}), "stream", &numbers(3), true),
            (numbers(3), None, Step::Done)
        );
    }

    #[test]
    fn first_last() {
        let big = json!({"predicate": groovy("{ it > 2 }")});
        assert_eq!(
            selected(json!({"first": big}), "value", &numbers(5), false),
            (vec![], Some(json!(3)), Step::Done),
            "it ends at the first match"
        );
        assert_eq!(
            selected(json!({"first": {}}), "value", &[], true),
            (vec![], None, Step::Done),
            "closes empty by default"
        );
        assert_eq!(
            selected(
                json!({"first": {"empty": {"value": 0}}}),
                "value",
                &[],
                true
            )
            .1,
            Some(json!(0))
        );
        assert_eq!(
            selected(json!({"last": big}), "value", &numbers(5), true).1,
            Some(json!(5))
        );
        assert_eq!(
            selected(json!({"last": {}}), "value", &[], true).1,
            Some(Json::Null),
            "null by default"
        );
    }

    #[test]
    fn unique_keys() {
        let items = [json!(1), json!(2.0), json!(1.0), json!(3), json!(2)];
        assert_eq!(
            selected(json!({"unique": {}}), "stream", &items, true).0,
            [json!(1), json!(2.0), json!(3)],
            "the first of each key"
        );
        let keyed = json!({"unique": {"key": groovy("{ it % 2 }")}});
        assert_eq!(
            selected(keyed, "stream", &numbers(4), true).0,
            [json!(1), json!(2)]
        );
    }

    #[test]
    fn distinct_initial() {
        let items = [json!(null), json!(1), json!(1), json!(2), json!(1)];
        assert_eq!(
            selected(json!({"distinct": {}}), "stream", &items, true).0,
            [json!(null), json!(1), json!(2), json!(1)]
        );
        let legacy = json!({"distinct": {"initial": {"value": null}}});
        assert_eq!(
            selected(legacy, "stream", &items, true).0,
            [json!(1), json!(2), json!(1)],
            "an initial null key drops a first null"
        );
        let parity = json!({"distinct": {"key": groovy("{ it % 2 }")}});
        assert_eq!(
            selected(
                parity,
                "stream",
                &[json!(1), json!(3), json!(4), json!(5)],
                true
            )
            .0,
            [json!(1), json!(4), json!(5)],
            "keys of the previous input"
        );
    }

    #[test]
    fn until_boundary() {
        let stop = groovy("{ it == 3 }");
        assert_eq!(
            selected(
                json!({"until": {"predicate": stop}}),
                "stream",
                &numbers(5),
                false
            ),
            (numbers(2), None, Step::Done)
        );
        let inclusive = json!({"until": {"predicate": stop, "inclusive": true}});
        assert_eq!(
            selected(inclusive, "stream", &numbers(5), false).0,
            numbers(3)
        );
    }

    #[test]
    fn empty_fallback() {
        let fill = json!({"if_empty": {"value": {"value": []}}});
        assert_eq!(selected(fill.clone(), "stream", &[], true).0, [json!([])]);
        assert_eq!(
            selected(fill, "stream", &[json!(null)], true).0,
            [json!(null)],
            "a null item is no empty stream"
        );
        let body = json!({"if_empty": {"body": groovy("{ -> 'none' }")}});
        assert_eq!(selected(body, "stream", &[], true).0, [json!("none")]);
    }
}
