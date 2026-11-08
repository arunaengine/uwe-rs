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

/// How an op orders the items it collects; equal items keep their arrival order.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Sort {
    #[default]
    None,
    Natural,
    /// Arrival order.
    Index,
    /// By the hash of the value.
    Hash,
    /// By the hash of the value, with File contents.
    Deep,
    /// By the key that the body computes for an item.
    Key(Box<Body>),
    /// By the comparison of `left` and `right` that the body computes.
    Compare(Box<Body>),
}

impl Sort {
    pub fn is_none(&self) -> bool {
        *self == Sort::None
    }
}

/// Groups tuple items by the items at the `by` positions, like Nextflow `groupTuple`: each
/// other position becomes a list. A group emits when it has `size` items, or at the close.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupTuple {
    #[serde(default = "first_position", skip_serializing_if = "is_first")]
    pub by: Vec<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_body: Option<Body>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub remainder: bool,
    #[serde(default, skip_serializing_if = "Sort::is_none")]
    pub sort: Sort,
}

/// The older spelling of [`GroupTuple`].
pub type Group = GroupTuple;

/// Joins tuple items of `left` and `right` on the `by` positions, one to one and first in
/// first out per key, like Nextflow `join`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Join {
    #[serde(default = "first_position", skip_serializing_if = "is_first")]
    pub by: Vec<u32>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub remainder: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub mismatch_error: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub duplicate_error: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Body>,
}

/// Gives every pair of a `left` and a `right` item, with equal keys at `by` when it is set.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Combine {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<Vec<u32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Body>,
}

/// Pairs each `right` item with the `left` item of its key, which the key body computes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cross {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<Body>,
    #[serde(default, skip_serializing_if = "LeftRule::is_last")]
    pub left: LeftRule,
}

/// Which `left` item of a key a cross keeps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeftRule {
    First,
    #[default]
    Last,
    Error,
}

impl LeftRule {
    pub fn is_last(&self) -> bool {
        *self == LeftRule::Last
    }
}

/// Sends each item to the output of the first case whose condition holds, through the body
/// of that case; an item that no case takes is dropped.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Route {
    pub cases: Vec<RouteCase>,
}

/// One case of a route.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteCase {
    pub output: String,
    pub condition: Body,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Body>,
}

/// Gives each item to every output through that output's body; the results of one item are
/// committed together.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MultiMap {
    pub outputs: Vec<OutputBody>,
}

/// The body of one output of a multi-map.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputBody {
    pub output: String,
    pub body: Body,
    #[serde(default = "keep", skip_serializing_if = "NullRule::is_keep")]
    pub nulls: NullRule,
}

fn keep() -> NullRule {
    NullRule::Keep
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

fn first_position() -> Vec<u32> {
    vec![0]
}

fn is_first(by: &[u32]) -> bool {
    by == [0]
}
