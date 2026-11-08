//! The keyed stream ops that keep items per key: group_tuple gathers the fields of a key
//! into lists, and cross pairs each right item with the left item of its key.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{HashMap, VecDeque};

use serde_json::Value as Json;
use wir::Body;
use wir::reserved::{GroupTuple, LeftRule};

use crate::callback::Callbacks;
use crate::collecting::sorted;
use crate::engine::Context;
use crate::operator::{CHUNK, Operator, Pull, Step, Turn};
use crate::pairing::{Arrival, arrive};
use crate::streams::Failure;
use crate::values;

/// The items of one key that wait to be a group.
struct Group {
    items: Vec<Vec<Json>>,
    size: Option<usize>,
}

/// Groups tuple items by the fields at `by`.
pub struct Grouping {
    pub spec: GroupTuple,
    pub callbacks: Callbacks,
    groups: HashMap<String, Group>,
    order: Vec<String>,
}

impl Grouping {
    pub fn new(spec: GroupTuple) -> Grouping {
        Grouping {
            spec,
            callbacks: Callbacks::default(),
            groups: HashMap::new(),
            order: Vec::new(),
        }
    }

    /// The size of the group of an item: the fixed size, the size its body gives, or the
    /// size of a Nextflow group key.
    fn size_of(
        &mut self,
        turn: &Turn<'_>,
        context: &Context<'_>,
        item: &Json,
        key: &[Json],
    ) -> Result<Option<usize>, Failure> {
        let size = match (&self.spec.size, &self.spec.size_body) {
            (Some(size), _) => Some(*size as i64),
            (None, Some(body)) => {
                let given = (self.callbacks.call(
                    context,
                    body,
                    (&["item"], std::slice::from_ref(item)),
                    &turn.values,
                ))
                .map_err(|why| turn.failed(why))?;
                Some(given.as_i64().filter(|size| *size > 0).ok_or_else(|| {
                    turn.failed(format!("the size of a group is {given}, no positive int"))
                })?)
            }
            (None, None) => key.iter().find_map(|field| {
                (field.get("class")?.as_str()? == "GroupKey").then(|| field["size"].as_i64())?
            }),
        };
        Ok(size.map(|size| size as usize))
    }

    /// The tuple of a group: the key fields at their places and a list at each other place.
    fn tuple(
        &mut self,
        turn: &Turn<'_>,
        context: &Context<'_>,
        items: Vec<Vec<Json>>,
    ) -> Result<Json, Failure> {
        let width = items.first().map_or(0, Vec::len);
        let by: Vec<usize> = self
            .spec
            .by
            .iter()
            .map(|position| *position as usize)
            .collect();
        let mut fields = Vec::new();
        for position in 0..width {
            if by.contains(&position) {
                fields.push(items[0][position].clone());
                continue;
            }
            let column = items.iter().map(|item| item[position].clone()).collect();
            fields.push(Json::Array(sorted(
                column,
                &self.spec.sort,
                (&mut self.callbacks, context),
                turn,
            )?));
        }
        Ok(Json::Array(fields))
    }
}

impl Operator for Grouping {
    fn step(&mut self, turn: &mut Turn<'_>, context: &Context<'_>) -> Result<Step, Failure> {
        let mut moved = false;
        for _ in 0..CHUNK {
            let item = match turn.pull("source")? {
                Pull::Item(item) => item,
                Pull::Pending => return Ok(if moved { Step::Moved } else { Step::Idle }),
                Pull::Closed => break,
            };
            moved = true;
            let Json::Array(fields) = &item else {
                return Err(turn.failed(format!("the item {item} is no tuple")));
            };
            let (key, _) =
                crate::pairing::split_key(&item, &self.spec.by).map_err(|why| turn.failed(why))?;
            let size = self.size_of(turn, context, &item, &key)?;
            let name = values::canonical(&Json::Array(key));
            let group = self.groups.entry(name.clone()).or_insert_with(|| Group {
                items: Vec::new(),
                size,
            });
            if group.items.is_empty() {
                group.size = size;
                if !self.order.contains(&name) {
                    self.order.push(name.clone());
                }
            }
            if group.size != size {
                return Err(
                    turn.failed(format!("the items of the key {name} give different sizes"))
                );
            }
            if group
                .items
                .first()
                .is_some_and(|first| first.len() != fields.len())
            {
                return Err(
                    turn.failed(format!("the items of the key {name} have different widths"))
                );
            }
            group.items.push(fields.clone());
            if group.size.is_some_and(|size| group.items.len() == size) {
                let items = std::mem::take(&mut group.items);
                let tuple = self.tuple(turn, context, items)?;
                turn.emit("out", tuple);
            }
        }
        if !matches!(turn.peek("source")?, Pull::Closed) {
            return Ok(Step::Moved);
        }
        for name in std::mem::take(&mut self.order) {
            let Some(group) = self.groups.remove(&name) else {
                continue;
            };
            let partial = group.size.is_some();
            if !group.items.is_empty() && (!partial || self.spec.remainder) {
                let tuple = self.tuple(turn, context, group.items)?;
                turn.emit("out", tuple);
            }
        }
        Ok(Step::Done)
    }
}

/// Pairs each right item with the left item of its key, as `[left, right]`.
pub struct Crossing {
    pub key: Option<Body>,
    pub left: LeftRule,
    pub callbacks: Callbacks,
    kept: HashMap<String, Json>,
    waiting: HashMap<String, VecDeque<Json>>,
}

impl Crossing {
    pub fn new(key: Option<Body>, left: LeftRule) -> Crossing {
        Crossing {
            key,
            left,
            callbacks: Callbacks::default(),
            kept: HashMap::new(),
            waiting: HashMap::new(),
        }
    }

    /// The key of an item: what the key body gives, else its first field.
    fn key_of(
        &mut self,
        turn: &Turn<'_>,
        context: &Context<'_>,
        item: &Json,
    ) -> Result<String, Failure> {
        let key = match &self.key {
            Some(body) => (self.callbacks.call(
                context,
                body,
                (&["item"], std::slice::from_ref(item)),
                &turn.values,
            ))
            .map_err(|why| turn.failed(why))?,
            None => match item {
                Json::Array(fields) => fields.first().cloned().unwrap_or_default(),
                other => other.clone(),
            },
        };
        Ok(values::canonical(&key))
    }
}

impl Operator for Crossing {
    fn step(&mut self, turn: &mut Turn<'_>, context: &Context<'_>) -> Result<Step, Failure> {
        let mut moved = false;
        for _ in 0..CHUNK {
            let (side, item) = match arrive(turn, &["left", "right"])? {
                Arrival::Item(side, item) => (side, item),
                Arrival::Pending => break,
                Arrival::Closed => return Ok(Step::Done),
            };
            moved = true;
            let key = self.key_of(turn, context, &item)?;
            if side == 1 {
                match self.kept.get(&key) {
                    Some(left) => turn.emit("out", Json::Array(vec![left.clone(), item])),
                    None => self.waiting.entry(key).or_default().push_back(item),
                }
                continue;
            }
            match (self.kept.contains_key(&key), self.left) {
                (true, LeftRule::Error) => {
                    return Err(turn.failed(format!("the left key {key} comes twice")));
                }
                (true, LeftRule::First) => continue,
                _ => {}
            }
            for right in self.waiting.remove(&key).unwrap_or_default() {
                turn.emit("out", Json::Array(vec![item.clone(), right]));
            }
            self.kept.insert(key, item);
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

    /// What an op of `kind` gives for items committed in order, with its inputs closed.
    fn run(
        kind: Json,
        inputs: &[&str],
        items: &[(&str, Json)],
    ) -> (Vec<Json>, Result<Step, Failure>) {
        let node = node(kind, inputs, &[], "stream");
        let mut streams = wired(&node);
        let mut operator = make(&node).unwrap();
        for item in items {
            commit(&mut streams, std::slice::from_ref(item), &[]);
        }
        commit(&mut streams, &[], inputs);
        let (emitted, _, step) = steps(operator.as_mut(), &node, &mut streams, &Map::new());
        (emitted, step)
    }

    fn sources(items: &[Json]) -> Vec<(&'static str, Json)> {
        items.iter().map(|item| ("source", item.clone())).collect()
    }

    #[test]
    fn group_sizes() {
        let items = sources(&[
            json!(["a", 1]),
            json!(["b", 2]),
            json!(["a", 3]),
            json!(["a", 4]),
        ]);
        let (emitted, step) = run(json!({"group_tuple": {"size": 2}}), &["source"], &items);
        assert_eq!(step, Ok(Step::Done));
        assert_eq!(
            emitted,
            [json!(["a", [1, 3]])],
            "a full group emits, partial ones drop"
        );
        let (emitted, _) = run(
            json!({"group_tuple": {"size": 2, "remainder": true}}),
            &["source"],
            &items,
        );
        assert_eq!(
            emitted,
            [json!(["a", [1, 3]]), json!(["a", [4]]), json!(["b", [2]])],
            "a key starts again; the rest keep first-seen key order"
        );
        let key =
            |name: &str, size: u32| json!({"class": "GroupKey", "target": name, "size": size});
        let keyed = sources(&[json!([key("a", 2), 1]), json!([key("a", 2), 2])]);
        assert_eq!(
            run(json!({"group_tuple": {}}), &["source"], &keyed).0,
            [json!([key("a", 2), [1, 2]])],
            "a group key sets the size"
        );
        let computed =
            json!({"group_tuple": {"size_body": {"language": "groovy", "code": "{ it[1] }"}}});
        let (_, step) = run(
            computed,
            &["source"],
            &sources(&[json!(["a", 2]), json!(["a", 3])]),
        );
        assert!(step.unwrap_err().message.contains("different sizes"));
    }

    #[test]
    fn group_remainder() {
        let items = sources(&[
            json!(["b", 3, "z"]),
            json!(["a", 1, "y"]),
            json!(["b", 2, "x"]),
        ]);
        let (emitted, _) = run(
            json!({"group_tuple": {"sort": "natural"}}),
            &["source"],
            &items,
        );
        assert_eq!(
            emitted,
            [json!(["b", [2, 3], ["x", "z"]]), json!(["a", [1], ["y"]])],
            "first-seen key order, each column sorted"
        );
    }

    #[test]
    fn cross_policy() {
        let items = [
            ("right", json!(["a", "r1"])),
            ("left", json!(["a", 1])),
            ("right", json!(["a", "r2"])),
            ("left", json!(["a", 2])),
            ("right", json!(["a", "r3"])),
            ("right", json!(["b", "lost"])),
        ];
        let pair = |left: i64, right: &str| json!([["a", left], ["a", right]]);
        let (emitted, step) = run(json!({"cross": {}}), &["left", "right"], &items);
        assert_eq!(step, Ok(Step::Done));
        assert_eq!(
            emitted,
            [pair(1, "r1"), pair(1, "r2"), pair(2, "r3")],
            "last keeps the newest left"
        );
        let (emitted, _) = run(
            json!({"cross": {"left": "first"}}),
            &["left", "right"],
            &items,
        );
        assert_eq!(emitted, [pair(1, "r1"), pair(1, "r2"), pair(1, "r3")]);
        let (_, step) = run(
            json!({"cross": {"left": "error"}}),
            &["left", "right"],
            &items,
        );
        assert!(step.unwrap_err().message.contains("comes twice"));
    }
}
