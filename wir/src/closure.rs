//! Callbacks of ops: a named value graph or a closure of a source language, with the scope it
//! comes from and the values it captures, and the value rules an op compares items with.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The language of an `expr` or a closure. `js` is the CWL expression form, which closures
/// do not take; `javascript` is its new spelling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExprLanguage {
    Js,
    Javascript,
    Groovy,
    Python,
    Wdl,
    /// An Argo text with `{{...}}` tags, as the Argo runtime of uwe substitutes it.
    Argo,
}

/// The lexical scope of a closure in a pinned source: a half-open UTF-8 byte range.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub source: String,
    pub start: u64,
    pub end: u64,
}

/// A captured value: the value of an input of the op, or a fixed value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Binding {
    Input(String),
    Value(serde_json::Value),
}

/// A closure of a source language. Groovy, Python and JavaScript code yields a callable and
/// takes no `parameters`; WDL code is an expression over the named `parameters`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Closure {
    pub language: ExprLanguage,
    pub code: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parameters: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub library: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<Scope>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub captures: BTreeMap<String, Binding>,
}

/// The callback of an op: the name of a value graph, or a closure object.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Body {
    Graph(String),
    Closure(Closure),
}

impl Body {
    /// The name of the graph the body runs, when it is a graph.
    pub fn graph(&self) -> Option<&str> {
        match self {
            Body::Graph(name) => Some(name),
            Body::Closure(_) => None,
        }
    }
}

/// The rules an op compares, orders, adds and formats values by.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueRules {
    /// Structural equality, numeric and lexical order, as the README defines.
    #[default]
    Wir,
    Groovy,
    Python,
    Wdl,
    Javascript,
}

impl ValueRules {
    pub fn is_wir(&self) -> bool {
        *self == ValueRules::Wir
    }
}
