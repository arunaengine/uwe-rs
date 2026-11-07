//! Reserved op kinds: their fields are fixed, their semantics are open. See the crate README.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde::{Deserialize, Serialize};

/// A pure function in a named expression language (`js`, `python`, `groovy`). Open: which
/// languages, numeric overflow, and how the function sees File values.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expr {
    pub language: String,
    pub code: String,
}

/// Runs the `body` graph once per item of the inputs in `over`. Open: each scatter mode,
/// per-round reads of streams and Nextflow `each`, output order, and failure with `min_success`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Map {
    pub body: String,
    pub over: Vec<String>,
    #[serde(default, skip_serializing_if = "Scatter::is_dot")]
    pub mode: Scatter,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallelism: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_success: Option<u32>,
}

/// How a `map` combines several inputs, as CWL `scatterMethod`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scatter {
    #[default]
    Dot,
    FlatCross,
    NestedCross,
}

/// Turns a stream into one array value once the stream closes. Open: order and what an
/// error inside the stream does.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Collect {}

/// Keeps the items for which the `body` graph returns true. Open: order and end of stream.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Filter {
    pub body: String,
}

/// Emits the items of each array item. Open: depth and empty arrays.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Flatten {}

/// Combines several streams into one. Open: completion order against fair order.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Merge {}

/// Pairs items by position. Open: unequal lengths; `remainder` keeps unpaired items.
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

/// Runs `body` only when the bool input `condition` is true; outputs are optional. Open: a
/// skipped status against null outputs, and CWL `when` with scatter.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Branch {
    pub body: String,
    pub condition: String,
}

/// Picks among optional inputs, like CWL `pickValue`. Open: the error when nothing is set.
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

impl Scatter {
    pub fn is_dot(&self) -> bool {
        *self == Scatter::Dot
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}
