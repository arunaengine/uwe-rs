//! The fields of the stream and control ops, which have defined semantics in the crate
//! README, and of the value ops `select`, `merge` and `flatten`.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde::{Deserialize, Serialize};

use crate::closure::Body;

/// Turns a stream into one array value once the stream closes. Open: order and what an
/// error inside the stream does.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Collect {}

/// Turns a value into a stream: the value as one item, or with `each` the items of an array.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Emit {
    #[serde(default, skip_serializing_if = "is_false")]
    pub each: bool,
}

/// Calls the body once per item, in source order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transform {
    pub body: Body,
    #[serde(default, skip_serializing_if = "NullRule::is_drop")]
    pub nulls: NullRule,
}

/// What an op does with a null result.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NullRule {
    #[default]
    Drop,
    Keep,
}

impl NullRule {
    pub fn is_drop(&self) -> bool {
        *self == NullRule::Drop
    }

    pub fn is_keep(&self) -> bool {
        *self == NullRule::Keep
    }
}

/// Keeps the items for which the body is true, in order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Filter {
    pub body: Body,
}

/// Applies the optional body, then gives the items of one collection level of each result.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlatMap {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Body>,
}

/// Gives the items of each array item: one level deep, or all levels with `depth: all`. On
/// streams, `drop_null` drops a null source item.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Flatten {
    #[serde(default, skip_serializing_if = "Depth::is_one")]
    pub depth: Depth,
    #[serde(default, skip_serializing_if = "is_false")]
    pub drop_null: bool,
}

/// How many array levels a flatten expands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Depth {
    #[default]
    One,
    All,
}

impl Depth {
    pub fn is_one(&self) -> bool {
        *self == Depth::One
    }
}

/// Gives each available item of its streams in arrival order; each stream keeps its order.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mix {}

/// Gives all items of its first stream, then of the next, in input order.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Concat {}

/// Gives the array of its input values, or in round mode one result per round: an item of
/// every stream with every value, through the optional body.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Merge {
    #[serde(default, skip_serializing_if = "is_false")]
    pub rounds: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Body>,
}

impl Merge {
    /// Whether the op takes rounds: it has a stream port or sets `rounds`.
    pub fn takes_rounds(&self, streams: bool) -> bool {
        self.rounds || streams
    }
}

/// Pairs items by position into tuples; with `remainder` up to the longest input, padded
/// with null.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Zip {
    #[serde(default, skip_serializing_if = "is_false")]
    pub remainder: bool,
}

/// Pairs every item of one input with every item of the other. Open: order and empty input.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cross {}

/// Groups tuple items by the items at the `by` positions, like Nextflow `groupTuple`.
/// Open: `size`, `remainder`, and the order of groups.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Group {
    pub by: Vec<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u32>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub remainder: bool,
}

/// Joins tuple items of two inputs on the `by` positions, like Nextflow `join`. Open:
/// duplicate keys, mismatch errors and order.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Join {
    pub by: Vec<u32>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub remainder: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub fail_on_mismatch: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub fail_on_duplicate: bool,
}

/// Picks among the items of its one array input, like CWL `pickValue`; the README defines it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Select {
    pub rule: Pick,
}

/// The `select` rules: first set value, the only set value, all set values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pick {
    First,
    Only,
    All,
}

/// Repeats `body` while its bool output `condition` is true, passing `carry` outputs to
/// inputs. Open: the first check, outputs of all rounds, and limits.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Loop {
    pub body: String,
    pub condition: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub carry: Vec<Carry>,
}

/// One value passed from a body output to a body input of the next round.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Carry {
    pub output: String,
    pub input: String,
}

/// Runs a graph that the input `plan` holds as a wir graph value, made at run time. Open:
/// stable instance ids, and reconciling with finished work.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dynamic {
    pub plan: String,
}

fn is_false(value: &bool) -> bool {
    !*value
}
