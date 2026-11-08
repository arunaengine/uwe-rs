//! The stream ops with named outputs: route sends each item to the output of its first
//! matching case, and multi_map gives each item to every output at once.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use wir::reserved::{NullRule, OutputBody, RouteCase};

use crate::callback::Callbacks;
use crate::engine::Context;
use crate::operator::{CHUNK, Operator, Pull, Step, Turn};
use crate::streams::Failure;
use crate::transforms::truth;

/// A route or a multi-map, with the callbacks of its bodies.
pub enum Routing {
    Route(Vec<RouteCase>, Callbacks),
    Multi(Vec<OutputBody>, Callbacks),
}

impl Operator for Routing {
    fn step(&mut self, turn: &mut Turn<'_>, context: &Context<'_>) -> Result<Step, Failure> {
        let mut moved = false;
        for _ in 0..CHUNK {
            let item = match turn.pull("source")? {
                Pull::Item(item) => item,
                Pull::Pending => break,
                Pull::Closed => return Ok(Step::Done),
            };
            moved = true;
            let values = turn.values.clone();
            let call = |callbacks: &mut Callbacks, body, item| {
                let called = callbacks.call(
                    context,
                    body,
                    (&["item"], std::slice::from_ref(item)),
                    &values,
                );
                called.map_err(|why| turn.failed(why))
            };
            match self {
                Routing::Route(cases, callbacks) => {
                    for case in cases.iter() {
                        let held = call(callbacks, &case.condition, &item)?;
                        match truth(&case.condition, &held) {
                            Some(false) => continue,
                            Some(true) => {}
                            None => {
                                return Err(turn.failed(format!(
                                    "the case of {} gave {held}, no bool",
                                    case.output
                                )));
                            }
                        }
                        let value = match &case.body {
                            Some(body) => call(callbacks, body, &item)?,
                            None => item.clone(),
                        };
                        turn.emit(&case.output, value);
                        break;
                    }
                }
                Routing::Multi(outputs, callbacks) => {
                    // The outputs of one item are given together, or none of them.
                    let mut given = Vec::new();
                    for output in outputs.iter() {
                        let value = call(callbacks, &output.body, &item)?;
                        if !(value.is_null() && output.nulls == NullRule::Drop) {
                            given.push((output.output.clone(), value));
                        }
                    }
                    for (output, value) in given {
                        turn.emit(&output, value);
                    }
                }
            }
        }
        Ok(if moved { Step::Moved } else { Step::Idle })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, json};

    use crate::mixing::tests::{commit, node, steps, wired};
    use crate::operator::{Step, make};
    use crate::streams::StreamId;

    fn groovy(code: &str) -> serde_json::Value {
        json!({"language": "groovy", "code": code})
    }

    #[test]
    fn route_first() {
        let cases = json!([{"output": "small", "condition": groovy("{ it < 10 }")},
            {"output": "even", "condition": groovy("{ it % 2 == 0 }"), "body": groovy("{ it.intdiv(2) }")},
            {"output": "small", "condition": groovy("{ true }")}]);
        let mut node = node(
            json!({"route": {"cases": cases}}),
            &["source"],
            &[],
            "stream",
        );
        let port = |name: &str| json!({"name": name, "type": "dynamic", "flow": "stream"});
        node.op.outputs = vec![
            serde_json::from_value(port("small")).unwrap(),
            serde_json::from_value(port("even")).unwrap(),
        ];
        let mut streams = wired(&node);
        let mut operator = make(&node).unwrap();
        let items = [
            ("source", json!(3)),
            ("source", json!(12)),
            ("source", json!(15)),
        ];
        commit(&mut streams, &items, &["source"]);
        let (emitted, _, step) = steps(operator.as_mut(), &node, &mut streams, &Map::new());
        assert_eq!(step, Ok(Step::Done));
        assert_eq!(
            emitted,
            [json!(3), json!(6), json!(15)],
            "the first case that holds, through its body"
        );
        let count = |name: &str| streams.channels[&StreamId(format!("op#{name}"))].next;
        assert_eq!((count("small"), count("even")), (2, 1));
    }

    #[test]
    fn multi_map_atomic() {
        let outputs = json!([{"output": "out", "body": groovy("{ it * 2 }")},
            {"output": "out", "body": groovy("{ 10 / it }"), "nulls": "drop"}]);
        let node = node(
            json!({"multi_map": {"outputs": outputs}}),
            &["source"],
            &[],
            "stream",
        );
        let mut streams = wired(&node);
        let mut operator = make(&node).unwrap();
        commit(
            &mut streams,
            &[("source", json!(5)), ("source", json!(0))],
            &["source"],
        );
        let (emitted, _, step) = steps(operator.as_mut(), &node, &mut streams, &Map::new());
        assert_eq!(
            emitted,
            [json!(10), json!(2.0)],
            "the first item gives both outputs"
        );
        let failure = step.unwrap_err();
        assert!(
            failure.message.contains("Division"),
            "the second item fails and gives none: {}",
            failure.message
        );
    }
}
