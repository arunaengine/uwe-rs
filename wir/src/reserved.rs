//! Op kinds whose fields are fixed and whose semantics stay open, apart from `select`, and
//! `merge` and `flatten` on values. See the crate README.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde::{Deserialize, Serialize};

use crate::closure::Body;

/// Turns a stream into one array value once the stream closes. Open: order and what an
/// error inside the stream does.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Collect {}

/// Keeps the items for which the `body` graph returns true. Open: order and end of stream.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Filter {
    pub body: Body,
}

/// Gives the items of each array item, one level deep, as the README defines for values.
/// Open: the same on streams.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Flatten {}

/// Gives the array of its input values, as the README defines for values. Open on streams:
/// completion order against fair order.
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
