//! Suggestions of tools that can follow an output or produce an input, best first.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cmp::Reverse;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::connection::{self, Check, Supply};
use crate::definition::{Definition, InputPort};
use crate::edam::hierarchy;
use crate::finding::Fit;

/// What else ranks the suggestions besides the format fit.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ranking {
    /// EDAM operations the user looks for; tools that do one of them, or a kind of one, come
    /// first. Empty ranks all tools alike.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operations: Vec<String>,
    /// How often each tool id is used in other workflows.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub uses: BTreeMap<String, u64>,
}

/// A port of a tool that fits, or may fit, with the check of the connection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Suggestion {
    pub tool: String,
    pub version: String,
    pub port: String,
    pub check: Check,
}

/// The inputs of `tools` that can take `supply`, best first: by format fit (exact, more
/// general, unknown), then EDAM operation, curation level, uses, and the answer of the check.
pub fn followers(supply: &Supply, tools: &[Definition], ranking: &Ranking) -> Vec<Suggestion> {
    let mut found = Vec::new();
    for tool in tools {
        for port in &tool.inputs {
            found.push((
                tool,
                suggestion(tool, &port.name, connection::check(supply, port)),
            ));
        }
    }
    ranked(found, ranking)
}

/// The outputs of `tools` that can feed `port`, best first, ranked as [`followers`] ranks.
/// A format that follows an input stands for the formats that input takes.
pub fn producers(port: &InputPort, tools: &[Definition], ranking: &Ranking) -> Vec<Suggestion> {
    let mut found = Vec::new();
    for tool in tools {
        for output in &tool.outputs {
            let Some(supply) = Supply::output(tool, &output.name, &BTreeMap::new(), false) else {
                continue;
            };
            found.push((
                tool,
                suggestion(tool, &output.name, connection::check(&supply, port)),
            ));
        }
    }
    ranked(found, ranking)
}

fn suggestion(tool: &Definition, port: &str, check: Check) -> Suggestion {
    Suggestion {
        tool: tool.id.clone(),
        version: tool.version.clone(),
        port: port.to_owned(),
        check,
    }
}

/// Drops the suggestions that do not fit and sorts the others.
fn ranked(found: Vec<(&Definition, Suggestion)>, ranking: &Ranking) -> Vec<Suggestion> {
    let edam = hierarchy();
    let wanted = |tool: &Definition| {
        ranking.operations.is_empty()
            || tool.operations.iter().any(|operation| {
                ranking
                    .operations
                    .iter()
                    .any(|wanted| edam.is_kind(operation, wanted))
            })
    };
    let mut kept: Vec<_> = found
        .into_iter()
        .filter(|(_, suggestion)| suggestion.check.fit != Fit::DoesNotFit)
        .collect();
    kept.sort_by_cached_key(|(tool, suggestion)| {
        (
            suggestion.check.format,
            !wanted(tool),
            tool.curation,
            Reverse(ranking.uses.get(&tool.id).copied().unwrap_or(0)),
            suggestion.check.fit,
            suggestion.tool.clone(),
            suggestion.version.clone(),
            suggestion.port.clone(),
        )
    });
    kept.into_iter().map(|(_, suggestion)| suggestion).collect()
}
