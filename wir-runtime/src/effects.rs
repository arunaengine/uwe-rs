//! The stream op `view`, which prints each item through the print sink of the host.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::Value as Json;
use wir::Body;

use crate::callback::Callbacks;
use crate::engine::Context;
use crate::operator::{CHUNK, Operator, Pull, Step, Turn};
use crate::streams::Failure;

/// Prints each item, through its body, and forwards it; a value is printed once.
pub struct Viewing {
    pub body: Option<Body>,
    pub new_line: bool,
    pub stream: bool,
    pub callbacks: Callbacks,
}

impl Viewing {
    fn show(&mut self, turn: &Turn<'_>, context: &Context<'_>, item: &Json) -> Result<(), Failure> {
        let shown = match &self.body {
            Some(body) => {
                let body = body.clone();
                (self.callbacks.call(
                    context,
                    &body,
                    (&["item"], std::slice::from_ref(item)),
                    &turn.values,
                ))
                .map_err(|why| turn.failed(why))?
            }
            None => item.clone(),
        };
        let mut text = match shown {
            Json::String(text) => text,
            other => other.to_string(),
        };
        if self.new_line {
            text.push('\n');
        }
        context.printer.print(&text);
        Ok(())
    }
}

impl Operator for Viewing {
    fn step(&mut self, turn: &mut Turn<'_>, context: &Context<'_>) -> Result<Step, Failure> {
        if !self.stream {
            let source = turn.values.get("source").cloned().unwrap_or_default();
            self.show(turn, context, &source)?;
            turn.bind("out", source);
            return Ok(Step::Done);
        }
        let mut moved = false;
        for _ in 0..CHUNK {
            match turn.pull("source")? {
                Pull::Item(item) => {
                    self.show(turn, context, &item)?;
                    turn.emit("out", item);
                    moved = true;
                }
                Pull::Pending => break,
                Pull::Closed => return Ok(Step::Done),
            }
        }
        Ok(if moved { Step::Moved } else { Step::Idle })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::{Map, Value as Json, json};

    use crate::driver::Printer;
    use crate::mixing::tests::{commit, node, steps_with, wired};
    use crate::operator::{Step, make};
    use crate::streams::{ReaderId, StreamId, StreamItem};

    fn groovy(code: &str) -> Json {
        json!({"language": "groovy", "code": code})
    }

    #[test]
    fn effect_replay() {
        let node = node(
            json!({"view": {"body": groovy("{ \"n=$it\" }")}}),
            &["source"],
            &[],
            "stream",
        );
        let mut streams = wired(&node);
        let mut operator = make(&node).unwrap();
        commit(
            &mut streams,
            &[("source", json!(1)), ("source", json!(2))],
            &["source"],
        );
        let printed = Arc::new(Mutex::new(String::new()));
        let sink = printed.clone();
        let printer = Printer::new(move |text| sink.lock().unwrap().push_str(text));
        let (emitted, _, step) = steps_with(
            operator.as_mut(),
            &node,
            &mut streams,
            &Map::new(),
            &printer,
        );
        assert_eq!(
            (emitted, step),
            (vec![json!(1), json!(2)], Ok(Step::Done)),
            "view forwards the items"
        );
        assert_eq!(
            *printed.lock().unwrap(),
            "n=1\nn=2\n",
            "through the print sink of the host"
        );
        let late = ReaderId {
            owner: "late".into(),
            input: "x".into(),
        };
        let out = StreamId("op#out".into());
        streams
            .subscribe(&out, late.clone(), wir::Subscription::All)
            .unwrap();
        let first: Option<StreamItem> = match streams.peek(&late).unwrap() {
            crate::streams::Read::Item(item) => Some(item),
            _ => None,
        };
        assert_eq!(first.map(|item| item.value), Some(json!(1)));
        assert_eq!(
            *printed.lock().unwrap(),
            "n=1\nn=2\n",
            "a late reader prints nothing again"
        );
    }
}
