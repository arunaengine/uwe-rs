//! The stream op that reduces a stream to one value at its close: a count, a sum, the least
//! or greatest item, or a fold.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cmp::Ordering;

use serde_json::Value as Json;
use wir::reserved::{EmptyRule, FoldNull, Reduce, Reduction};

use crate::callback::Callbacks;
use crate::engine::Context;
use crate::operator::{CHUNK, Operator, Pull, Step, Turn};
use crate::streams::Failure;
use crate::values;

/// Reduces `source`, with the state so far.
pub struct Reducing {
    pub spec: Reduce,
    pub callbacks: Callbacks,
    /// The value so far: the count, the sum, the kept item with its key, or the fold state.
    state: Option<(Json, Json)>,
    count: i64,
    /// Whether an item came.
    seen: bool,
}

impl Reducing {
    pub fn new(spec: Reduce) -> Reducing {
        let state = spec
            .seed
            .as_ref()
            .map(|seed| (seed.value.clone(), Json::Null));
        Reducing {
            spec,
            callbacks: Callbacks::default(),
            state,
            count: 0,
            seen: false,
        }
    }

    fn call(
        &mut self,
        turn: &Turn<'_>,
        context: &Context<'_>,
        names: &[&str],
        arguments: &[Json],
    ) -> Result<Json, Failure> {
        let body = match names.len() {
            2 if self.spec.rule != Reduction::Fold => self.spec.compare.as_ref(),
            _ => self.spec.body.as_ref(),
        };
        let Some(body) = body else {
            return Ok(arguments[0].clone());
        };
        let body = body.clone();
        (self
            .callbacks
            .call(context, &body, (names, arguments), &turn.values))
        .map_err(|why| turn.failed(why))
    }

    fn take(&mut self, turn: &Turn<'_>, context: &Context<'_>, item: Json) -> Result<(), Failure> {
        match self.spec.rule {
            Reduction::Count => {
                let counted = match self.spec.body.clone() {
                    Some(body) => {
                        let held = self.call(turn, context, &["item"], &[item])?;
                        crate::transforms::truth(&body, &held).ok_or_else(|| {
                            turn.failed(format!("the condition gave {held}, no bool"))
                        })?
                    }
                    None => true,
                };
                self.count += i64::from(counted);
            }
            Reduction::Sum => {
                let value = self.call(turn, context, &["item"], &[item])?;
                if !value.is_null() {
                    let sum = match self.state.take() {
                        Some((sum, _)) => add(&sum, &value).map_err(|why| turn.failed(why))?,
                        None => add(&Json::from(0), &value).map_err(|why| turn.failed(why))?,
                    };
                    self.state = Some((sum, Json::Null));
                }
            }
            Reduction::Min | Reduction::Max => {
                let key = match &self.spec.compare {
                    Some(_) => Json::Null,
                    None => self.call(turn, context, &["item"], std::slice::from_ref(&item))?,
                };
                let replace = match &self.state {
                    None => true,
                    Some((kept, kept_key)) => {
                        let order = match &self.spec.compare {
                            Some(_) => {
                                let order = self.call(
                                    turn,
                                    context,
                                    &["left", "right"],
                                    &[item.clone(), kept.clone()],
                                )?;
                                let number = order.as_f64().ok_or_else(|| {
                                    turn.failed(format!("the comparison gave {order}, no number"))
                                })?;
                                number.partial_cmp(&0.0).unwrap_or(Ordering::Equal)
                            }
                            None => {
                                values::compare(&key, kept_key).map_err(|why| turn.failed(why))?
                            }
                        };
                        // The least keeps the first of equal items, the greatest the last.
                        match self.spec.rule {
                            Reduction::Min => order == Ordering::Less,
                            _ => order != Ordering::Less,
                        }
                    }
                };
                if replace {
                    self.state = Some((item, key));
                }
            }
            Reduction::Fold => match self.state.take() {
                None => self.state = Some((item, Json::Null)),
                Some((state, _)) => {
                    let next =
                        self.call(turn, context, &["state", "item"], &[state.clone(), item])?;
                    let next = match (next, self.spec.nulls) {
                        (Json::Null, FoldNull::KeepState) => state,
                        (next, _) => next,
                    };
                    self.state = Some((next, Json::Null));
                }
            },
        }
        Ok(())
    }

    /// Binds the result at the close, or what the empty rule says for an empty stream: by
    /// default zero for a count, the seed of a fold, and null otherwise.
    fn close(&mut self, turn: &mut Turn<'_>) -> Result<(), Failure> {
        let result = match (self.seen, &self.spec.empty) {
            (true, _) => Some(match self.spec.rule {
                Reduction::Count => Json::from(self.count),
                _ => self
                    .state
                    .take()
                    .map(|(value, _)| value)
                    .unwrap_or_default(),
            }),
            (false, EmptyRule::Default) => Some(match self.spec.rule {
                Reduction::Count => Json::from(0),
                Reduction::Fold => self.state.take().map(|(seed, _)| seed).unwrap_or_default(),
                _ => Json::Null,
            }),
            (false, EmptyRule::Null) => Some(Json::Null),
            (false, EmptyRule::Value(value)) => Some(value.clone()),
            (false, EmptyRule::Close) => None,
            (false, EmptyRule::Error) => return Err(turn.failed("the stream closed empty")),
        };
        match result {
            Some(value) => turn.bind("out", value),
            None => turn.close_empty("out"),
        }
        Ok(())
    }
}

impl Operator for Reducing {
    fn step(&mut self, turn: &mut Turn<'_>, context: &Context<'_>) -> Result<Step, Failure> {
        let mut moved = false;
        for _ in 0..CHUNK {
            match turn.pull("source")? {
                Pull::Item(item) => {
                    self.seen = true;
                    self.take(turn, context, item)?;
                    // With `steps`, the value after each item is an item of the output.
                    if self.spec.steps {
                        let value = match self.spec.rule {
                            Reduction::Count => Json::from(self.count),
                            _ => {
                                (self.state.as_ref()).map_or(Json::Null, |(value, _)| value.clone())
                            }
                        };
                        turn.emit("out", value);
                    }
                    moved = true;
                }
                Pull::Pending => break,
                Pull::Closed if self.spec.steps => return Ok(Step::Done),
                Pull::Closed => {
                    self.close(turn)?;
                    return Ok(Step::Done);
                }
            }
        }
        Ok(if moved { Step::Moved } else { Step::Idle })
    }
}

/// The sum of two numbers by the wir rules: ints stay ints and fail on overflow, an int and a
/// float give a float, and other values do not add.
pub fn add(left: &Json, right: &Json) -> Result<Json, String> {
    match (left, right) {
        (Json::Number(a), Json::Number(b)) => match (a.as_i64(), b.as_i64()) {
            (Some(a), Some(b)) => a
                .checked_add(b)
                .map(Json::from)
                .ok_or_else(|| format!("{a} + {b} is out of the int range")),
            _ => {
                let sum = a.as_f64().unwrap_or_default() + b.as_f64().unwrap_or_default();
                serde_json::Number::from_f64(sum)
                    .map(Json::Number)
                    .ok_or_else(|| format!("{a} + {b} is no finite number"))
            }
        },
        _ => Err(format!("cannot add {left} and {right}")),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, Value as Json, json};

    use crate::mixing::tests::{commit, node, steps, wired};
    use crate::operator::{Step, make};

    /// What a reduce with `fields` binds for the items of a closed stream, or why it failed.
    fn reduced(fields: Json, items: &[Json]) -> Result<Option<Json>, String> {
        let node = node(json!({"reduce": fields}), &["source"], &[], "value");
        let mut streams = wired(&node);
        let mut operator = make(&node).unwrap();
        let items: Vec<(&str, Json)> = items.iter().map(|item| ("source", item.clone())).collect();
        commit(&mut streams, &items, &["source"]);
        let (_, bound, step) = steps(operator.as_mut(), &node, &mut streams, &Map::new());
        match step {
            Ok(Step::Done) => Ok(bound),
            Ok(other) => panic!("the reduce did not end: {other:?}"),
            Err(failure) => Err(failure.message),
        }
    }

    #[test]
    fn reduce_steps() {
        let node = node(
            json!({"reduce": {"rule": "sum", "steps": true}}),
            &["source"],
            &[],
            "stream",
        );
        let mut streams = wired(&node);
        let mut operator = make(&node).unwrap();
        let items = [
            ("source", json!(1)),
            ("source", json!(2)),
            ("source", json!(3)),
        ];
        commit(&mut streams, &items, &["source"]);
        let (emitted, bound, step) = steps(operator.as_mut(), &node, &mut streams, &Map::new());
        assert_eq!(step, Ok(Step::Done));
        assert_eq!(
            emitted,
            [json!(1), json!(3), json!(6)],
            "the sum after every item, like scan"
        );
        assert_eq!(bound, None, "no value is bound at the close");
    }

    fn groovy(code: &str) -> Json {
        json!({"language": "groovy", "code": code})
    }

    #[test]
    fn reduce_empty() {
        assert_eq!(reduced(json!({"rule": "count"}), &[]), Ok(Some(json!(0))));
        for rule in ["sum", "min", "max", "fold"] {
            let body = if rule == "fold" {
                json!({"body": groovy("{ s, x -> s + x }")})
            } else {
                json!({})
            };
            let mut fields = json!({"rule": rule});
            fields
                .as_object_mut()
                .unwrap()
                .extend(body.as_object().cloned().unwrap());
            assert_eq!(
                reduced(fields.clone(), &[]),
                Ok(Some(Json::Null)),
                "{rule} gives null by default"
            );
            fields["empty"] = json!("close");
            assert_eq!(
                reduced(fields.clone(), &[]),
                Ok(None),
                "{rule} closes empty"
            );
            fields["empty"] = json!({"value": 7});
            assert_eq!(reduced(fields.clone(), &[]), Ok(Some(json!(7))));
            fields["empty"] = json!("error");
            assert!(reduced(fields, &[]).is_err());
        }
        let seeded =
            json!({"rule": "fold", "body": groovy("{ s, x -> s + x }"), "seed": {"value": 10}});
        assert_eq!(
            reduced(seeded, &[]),
            Ok(Some(json!(10))),
            "an empty fold gives its seed"
        );
    }

    #[test]
    fn reduce_nulls() {
        let add = groovy("{ s, x -> x == null ? null : s + x }");
        let kept = json!({"rule": "fold", "body": add, "seed": {"value": 0}});
        assert_eq!(
            reduced(kept, &[json!(1), json!(null)]),
            Ok(Some(Json::Null)),
            "keep takes a null state"
        );
        let state =
            json!({"rule": "fold", "body": add, "seed": {"value": 0}, "nulls": "keep_state"});
        assert_eq!(
            reduced(state, &[json!(1), json!(null), json!(2)]),
            Ok(Some(json!(3))),
            "keep_state ignores it"
        );
        let null_seed = json!({"rule": "fold", "body": groovy("{ s, x -> s == null ? x : s * x }"), "seed": {"value": null}});
        assert_eq!(
            reduced(null_seed, &[json!(2), json!(3)]),
            Ok(Some(json!(6))),
            "a null seed is a seed"
        );
        assert_eq!(
            reduced(json!({"rule": "sum"}), &[json!(null), json!(null)]),
            Ok(Some(Json::Null)),
            "all null sums to null"
        );
    }

    #[test]
    fn reduce_values() {
        assert_eq!(
            reduced(json!({"rule": "sum"}), &[json!(1), json!(2.5), json!(null)]),
            Ok(Some(json!(3.5)))
        );
        let overflow = reduced(json!({"rule": "sum"}), &[json!(i64::MAX), json!(1)]);
        assert!(overflow.unwrap_err().contains("out of the int range"));
        assert!(
            reduced(json!({"rule": "sum"}), &[json!(1), json!("a")])
                .unwrap_err()
                .contains("cannot add")
        );
        let pairs = [json!(["a", 2]), json!(["b", 1]), json!(["c", 1])];
        let by_count = groovy("{ it[1] }");
        assert_eq!(
            reduced(json!({"rule": "min", "body": by_count}), &pairs),
            Ok(Some(json!(["b", 1]))),
            "the first least"
        );
        assert_eq!(
            reduced(
                json!({"rule": "max", "body": by_count}),
                &[json!(["a", 1]), json!(["b", 1])]
            ),
            Ok(Some(json!(["b", 1]))),
            "the last greatest"
        );
        let compare =
            json!({"rule": "max", "compare": groovy("{ a, b -> a.size() <=> b.size() }")});
        assert_eq!(
            reduced(compare, &[json!("aa"), json!("b"), json!("cc")]),
            Ok(Some(json!("cc")))
        );
        let counted = json!({"rule": "count", "body": groovy("{ it > 1 }")});
        assert_eq!(
            reduced(counted, &[json!(1), json!(2), json!(3)]),
            Ok(Some(json!(2)))
        );
    }
}
