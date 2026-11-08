//! The stream ops that gather a stream into one value at its close: collect and to_list, and
//! the stable sort rules that collecting ops share.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cmp::Ordering;

use serde_json::Value as Json;
use wir::Body;
use wir::reserved::Sort;

use crate::callback::Callbacks;
use crate::engine::Context;
use crate::operator::{CHUNK, Operator, Pull, Step, Turn};
use crate::streams::Failure;
use crate::values;

/// Gathers the items of `source`: to_list as they are, collect through its body, one level
/// flat and sorted. Collect closes empty when it gathered nothing.
pub struct Collecting {
    pub body: Option<Body>,
    pub flat: bool,
    pub sort: Sort,
    /// A to_list, which binds `[]` for an empty stream.
    pub list: bool,
    pub items: Vec<Json>,
    pub callbacks: Callbacks,
}

impl Operator for Collecting {
    fn step(&mut self, turn: &mut Turn<'_>, context: &Context<'_>) -> Result<Step, Failure> {
        let mut moved = false;
        for _ in 0..CHUNK {
            let item = match turn.pull("source")? {
                Pull::Item(item) => item,
                Pull::Pending => return Ok(if moved { Step::Moved } else { Step::Idle }),
                Pull::Closed => break,
            };
            moved = true;
            if self.list {
                self.items.push(item);
                continue;
            }
            let value = match &self.body {
                Some(body) => {
                    (self
                        .callbacks
                        .call(context, body, (&["item"], &[item]), &turn.values))
                    .map_err(|why| turn.failed(why))?
                }
                None => item,
            };
            match value {
                Json::Array(inner) if self.flat => self.items.extend(inner),
                other => self.items.push(other),
            }
        }
        if !matches!(turn.peek("source")?, Pull::Closed) {
            return Ok(Step::Moved);
        }
        let items = std::mem::take(&mut self.items);
        if items.is_empty() && !self.list {
            turn.close_empty("out");
            return Ok(Step::Done);
        }
        let sorted = sorted(items, &self.sort, (&mut self.callbacks, context), turn)?;
        turn.bind("out", Json::Array(sorted));
        Ok(Step::Done)
    }
}

/// The items in the order of a sort rule; equal items keep their order.
pub fn sorted(
    mut items: Vec<Json>,
    sort: &Sort,
    (callbacks, context): (&mut Callbacks, &Context<'_>),
    turn: &Turn<'_>,
) -> Result<Vec<Json>, Failure> {
    let mut failed = None;
    let mut keep = |result: Result<Ordering, String>| match result {
        Ok(order) => order,
        Err(why) => {
            failed.get_or_insert(why);
            Ordering::Equal
        }
    };
    match sort {
        Sort::None | Sort::Index => {}
        Sort::Natural => items.sort_by(|a, b| keep(values::compare(a, b))),
        Sort::Hash => items.sort_by_cached_key(values::hash),
        Sort::Deep => {
            let mut keyed = Vec::new();
            for item in items {
                let hash = values::deep_hash(&item, context)
                    .ok_or_else(|| turn.failed("cannot read a file to sort by its content"))?;
                keyed.push((hash, item));
            }
            keyed.sort_by(|a, b| a.0.cmp(&b.0));
            items = keyed.into_iter().map(|(_, item)| item).collect();
        }
        Sort::Key(body) => {
            let mut keyed = Vec::new();
            for item in items {
                let key = callbacks.call(
                    context,
                    body,
                    (&["item"], std::slice::from_ref(&item)),
                    &turn.values,
                );
                keyed.push((key.map_err(|why| turn.failed(why))?, item));
            }
            keyed.sort_by(|a, b| keep(values::compare(&a.0, &b.0)));
            items = keyed.into_iter().map(|(_, item)| item).collect();
        }
        Sort::Compare(body) => items.sort_by(|a, b| {
            let pair = [a.clone(), b.clone()];
            let order = callbacks.call(context, body, (&["left", "right"], &pair), &turn.values);
            keep(order.and_then(|order| match order.as_f64() {
                Some(number) => Ok(number.partial_cmp(&0.0).unwrap_or(Ordering::Equal)),
                None => Err(format!("the comparison gave {order}, no number")),
            }))
        }),
    }
    match failed {
        Some(why) => Err(turn.failed(why)),
        None => Ok(items),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, json};

    use crate::mixing::tests::{commit, node, steps, wired};
    use crate::operator::{Step, make};

    /// What an op of `kind` binds for the items of a closed stream.
    fn gathered(kind: serde_json::Value, items: &[serde_json::Value]) -> Option<serde_json::Value> {
        let node = node(kind, &["source"], &[], "value");
        let mut streams = wired(&node);
        let mut operator = make(&node).unwrap();
        let items: Vec<(&str, serde_json::Value)> =
            items.iter().map(|item| ("source", item.clone())).collect();
        commit(&mut streams, &items, &["source"]);
        let (_, bound, step) = steps(operator.as_mut(), &node, &mut streams, &Map::new());
        assert_eq!(step, Ok(Step::Done));
        bound
    }

    #[test]
    fn collect_empty() {
        assert_eq!(
            gathered(json!({"collect": {}}), &[]),
            None,
            "collect closes empty"
        );
        assert_eq!(
            gathered(json!({"to_list": {}}), &[]),
            Some(json!([])),
            "to_list binds []"
        );
        let lists = [json!([1, 2]), json!([]), json!(3)];
        assert_eq!(
            gathered(json!({"to_list": {}}), &lists),
            Some(json!([[1, 2], [], 3])),
            "to_list keeps items whole"
        );
    }

    #[test]
    fn collect_order() {
        let lists = [json!([3, 1]), json!(2), json!([[4]])];
        assert_eq!(
            gathered(json!({"collect": {}}), &lists),
            Some(json!([3, 1, 2, [4]])),
            "one level flat"
        );
        assert_eq!(
            gathered(json!({"collect": {"flat": false}}), &lists),
            Some(json!([[3, 1], 2, [[4]]]))
        );
        let numbers = [json!(3), json!(1), json!(2)];
        assert_eq!(
            gathered(json!({"collect": {"sort": "natural"}}), &numbers),
            Some(json!([1, 2, 3]))
        );
        let groovy = |code: &str| json!({"language": "groovy", "code": code});
        let pairs = [json!(["b", 1]), json!(["a", 2]), json!(["b", 0])];
        let by_name = json!({"collect": {"flat": false, "sort": {"key": groovy("{ it[0] }")}}});
        assert_eq!(
            gathered(by_name, &pairs),
            Some(json!([["a", 2], ["b", 1], ["b", 0]])),
            "stable for equal keys"
        );
        let reversed = json!({"collect": {"sort": {"compare": groovy("{ a, b -> b <=> a }")}}});
        assert_eq!(gathered(reversed, &numbers), Some(json!([3, 2, 1])));
        let mapped = json!({"collect": {"body": groovy("{ [it, it * 10] }")}});
        assert_eq!(
            gathered(mapped, &[json!(1), json!(2)]),
            Some(json!([1, 10, 2, 20]))
        );
    }
}
