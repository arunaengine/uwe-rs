//! The fields of the stream and control ops, which have defined semantics in the crate
//! README, and of the value ops `select`, `merge` and `flatten`.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::closure::{Body, Closure};
use crate::document::Graph;
use crate::op::Emission;
pub use crate::splits::{
    ChunkFiles, CsvSplit, Decompress, Header, JsonSplit, Pair, RecordField, SequenceField,
    SequenceSplit, Split, SplitFormat, TextSplit,
};

/// Binds the array of the items of a stream once it closes: through the optional body,
/// flattened one level unless `flat` is false, and sorted. An empty result closes empty.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Collect {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Body>,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub flat: bool,
    #[serde(default, skip_serializing_if = "Sort::is_none")]
    pub sort: Sort,
}

impl Default for Collect {
    fn default() -> Self {
        Collect {
            body: None,
            flat: true,
            sort: Sort::None,
        }
    }
}

/// Binds the array of the items of a stream as they are once it closes; empty gives `[]`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToList {}

/// A fixed value, so that a null value differs from no value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Literal {
    pub value: serde_json::Value,
}

/// Reduces a stream to one value at its close: a count, a sum, the least or greatest item,
/// or a fold of the body over the items.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reduce {
    pub rule: Reduction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Body>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compare: Option<Body>,
    /// A null here is no seed form; a null seed is `{"value": null}`.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub seed: Option<Literal>,
    #[serde(default, skip_serializing_if = "EmptyRule::is_default")]
    pub empty: EmptyRule,
    #[serde(default, skip_serializing_if = "FoldNull::is_keep")]
    pub nulls: FoldNull,
    /// Whether the output is a stream of the value after every item, like nf-boost `scan`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub steps: bool,
}

/// The rules of a reduce.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reduction {
    Count,
    Sum,
    Min,
    Max,
    Fold,
}

/// What an op binds when its stream closes empty; `default` is the rule of the op.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum EmptyRule {
    #[default]
    Default,
    Close,
    Null,
    Error,
    Value(serde_json::Value),
}

impl EmptyRule {
    pub fn is_default(&self) -> bool {
        *self == EmptyRule::Default
    }
}

/// What a fold does with a null result of its body: keep it as the state, or keep the state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FoldNull {
    #[default]
    Keep,
    KeepState,
}

impl FoldNull {
    pub fn is_keep(&self) -> bool {
        *self == FoldNull::Keep
    }
}

/// Binds the first item for which the predicate holds, then stops reading.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct First {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predicate: Option<Body>,
    #[serde(default, skip_serializing_if = "EmptyRule::is_default")]
    pub empty: EmptyRule,
}

/// Binds the last item for which the predicate holds, at the close.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Last {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predicate: Option<Body>,
    #[serde(default, skip_serializing_if = "EmptyRule::is_default")]
    pub empty: EmptyRule,
}

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

/// Gives arrays of items: of `size` items after skipping `skip`, or from an item that
/// opens to one that closes. A partial buffer is given at the close with `remainder`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Buffer {
    pub rule: BufferRule,
    #[serde(default, skip_serializing_if = "is_false")]
    pub remainder: bool,
}

/// How a buffer cuts its stream.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum BufferRule {
    Size(BufferSize),
    Match(Box<BufferMatch>),
}

/// Buffers of `size` items, each after skipping `skip` items.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BufferSize {
    pub size: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub skip: u32,
}

/// Buffers from an item that `open` holds for, or from the start, to one that `close`
/// holds for, both included.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BufferMatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open: Option<Body>,
    pub close: Body,
}

/// Windows of `size` items, one opening every `step` items, by default `size`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Window {
    pub size: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<u32>,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub remainder: bool,
}

/// Gives the first `count` items, then stops reading; a negative count gives all.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Take {
    pub count: i64,
}

/// Keeps the first item of each key.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Unique {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<Body>,
}

/// Drops an item whose key equals the key of the item before it; `initial` is the key
/// before the first item, if there is one.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Distinct {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<Body>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub initial: Option<Literal>,
}

/// Gives the items before the first one the predicate holds for, which `inclusive` gives
/// too, then stops reading.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Until {
    pub predicate: Body,
    #[serde(default, skip_serializing_if = "is_false")]
    pub inclusive: bool,
}

/// Forwards its input, and gives `value` or the result of `body` when the input closes
/// empty; exactly one of the two is set.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IfEmpty {
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub value: Option<Literal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Body>,
}

/// Writes the entries of its items to files by name at the close, then gives the files in
/// name order. It writes files, so it declares `write_files`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollectFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Body>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<String>,
    #[serde(default = "by_hash", skip_serializing_if = "is_hash")]
    pub sort: Sort,
    #[serde(default, skip_serializing_if = "is_false")]
    pub new_line: bool,
    #[serde(default, skip_serializing_if = "is_none_u64")]
    pub skip: u64,
    #[serde(default, skip_serializing_if = "is_false")]
    pub keep_header: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<String>,
}

/// Prints each item, formatted by the optional body, and forwards it. It prints, so it
/// declares `log`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct View {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Body>,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub new_line: bool,
}

/// Calls `item` for each item before forwarding it, `close` once at a successful close and
/// `error` once at an error, which stays an error.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observe {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<Closure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub close: Option<Closure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Closure>,
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

/// Runs `body` once, then again while its bool output `condition` is true, passing each
/// `carry` output to its input; at most `limit` runs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Loop {
    pub body: String,
    pub condition: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub carry: Vec<Carry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
}

/// One value passed from a body output to a body input of the next round.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Carry {
    pub output: String,
    pub input: String,
}

/// Runs a region whose graph comes from plan updates on the input `plan`, or from a
/// language planner; exactly one of the two is set.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dynamic {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planner: Option<Planner>,
    #[serde(default, skip_serializing_if = "Emission::is_completion")]
    pub order: Emission,
}

/// A planner of a language that makes the graph of a region.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Planner {
    Snakemake(PlanRef),
}

/// The source, targets, profiles and configs a planner plans from, with the config values
/// and the options that pick jobs, as a `snakemake_job` builder has them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanRef {
    pub source: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub profiles: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub configs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_values: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub until: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub omit_from: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forcerun: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub forceall: bool,
}

/// One revision of the graph of a region, with the identities of its top-level ops.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanUpdate {
    pub revision: u64,
    pub graph: Graph,
    /// Empty: each op's path is its identity; else one key per top-level op.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<InstanceKey>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub final_plan: bool,
}

/// The identity of a top-level op of a plan.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstanceKey {
    pub op: String,
    pub identity: Identity,
}

/// A stable identity: a name, or a Snakemake rule with its wildcard values.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Identity {
    Named(String),
    Rule(RuleKey),
}

/// A Snakemake job: its source, rule and sorted wildcard values.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleKey {
    pub source: String,
    pub rule: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub wildcards: BTreeMap<String, String>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// Reads a field that is there, so that a JSON null is no `None`.
fn present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

fn by_hash() -> Sort {
    Sort::Hash
}

fn is_hash(sort: &Sort) -> bool {
    *sort == Sort::Hash
}

fn is_none_u64(value: &u64) -> bool {
    *value == 0
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}

fn yes() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

fn first_position() -> Vec<u32> {
    vec![0]
}

fn is_first(by: &[u32]) -> bool {
    by == [0]
}
