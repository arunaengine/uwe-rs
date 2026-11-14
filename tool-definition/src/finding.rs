//! The answers of the checks: fits, unknown or does not fit, with the reason per aspect.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde::{Deserialize, Serialize};

use crate::conditions::Evidence;

/// The answer of a check, from best to worst. Unknown warns and never refuses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fit {
    Fits,
    Unknown,
    DoesNotFit,
}

/// What a finding is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Aspect {
    Kind,
    Cardinality,
    Format,
    Condition,
    Parameter,
    /// The graph itself: missing tools, ports or connections, and cycles.
    Connection,
}

/// One answer of a check with its reason in one sentence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    pub aspect: Aspect,
    pub fit: Fit,
    pub reason: String,
    /// What decided a data condition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Evidence>,
}

impl Finding {
    pub(crate) fn new(aspect: Aspect, fit: Fit, reason: String) -> Finding {
        Finding {
            aspect,
            fit,
            reason,
            evidence: None,
        }
    }
}

/// The worst answer of the findings; fits when there are none.
pub fn worst(findings: &[Finding]) -> Fit {
    findings
        .iter()
        .map(|finding| finding.fit)
        .max()
        .unwrap_or(Fit::Fits)
}
