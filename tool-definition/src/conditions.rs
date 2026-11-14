//! Data conditions on a connection: what is known about the data, with its evidence.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde::{Deserialize, Serialize};

use crate::definition::{Condition, SortKey};
use crate::finding::{Aspect, Finding, Fit};

/// What is known about the data on a connection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fact {
    pub condition: Condition,
    /// False when the evidence shows that the condition does not hold.
    #[serde(default = "holds")]
    pub holds: bool,
    pub evidence: Evidence,
}

/// Where a fact comes from, from strongest to weakest: `{"checked": {"version": "..."}}`,
/// `"guaranteed"`, `"asserted"` or `"unknown"`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Evidence {
    /// A check of the data at this exact version.
    Checked {
        version: String,
    },
    /// The output declaration of the producing tool.
    Guaranteed,
    /// The user says so.
    Asserted,
    Unknown,
}

/// One finding per condition the port needs, decided by the strongest fact about it. When
/// facts of equal strength disagree, the one that does not hold wins; unknown evidence warns.
pub fn check(needs: &[Condition], facts: &[Fact]) -> Vec<Finding> {
    needs
        .iter()
        .map(|need| {
            let deciding = facts
                .iter()
                .filter(|fact| fact.evidence != Evidence::Unknown)
                .filter_map(|fact| answer(need, fact).map(|holds| (holds, fact)))
                .max_by_key(|(holds, fact)| (fact.evidence.strength(), !holds));
            let unknown = facts.iter().any(|fact| answer(need, fact).is_some());
            let (fit, reason, evidence) = match deciding {
                None if unknown => (
                    Fit::Unknown,
                    format!(
                        "only a source of unknown evidence speaks to whether the data {}; \
                         check the data or assert it",
                        describe(need)
                    ),
                    Evidence::Unknown,
                ),
                None => (
                    Fit::Unknown,
                    format!(
                        "nothing shows that the data {}; check the data or assert it",
                        describe(need)
                    ),
                    Evidence::Unknown,
                ),
                Some((true, fact)) => (
                    Fit::Fits,
                    format!(
                        "{} that the data {}",
                        fact.evidence.source(),
                        describe(need)
                    ),
                    fact.evidence.clone(),
                ),
                Some((false, fact)) if fact.condition == *need => (
                    Fit::DoesNotFit,
                    format!(
                        "{} that this does not hold: the data {}",
                        fact.evidence.source(),
                        describe(need)
                    ),
                    fact.evidence.clone(),
                ),
                Some((false, fact)) => (
                    Fit::DoesNotFit,
                    format!(
                        "{} that the data {}, and the port needs data that {}",
                        fact.evidence.source(),
                        describe(&fact.condition),
                        describe(need)
                    ),
                    fact.evidence.clone(),
                ),
            };
            Finding {
                evidence: Some(evidence),
                ..Finding::new(Aspect::Condition, fit, reason)
            }
        })
        .collect()
}

/// Whether `fact` says that `need` holds; `None` when it says nothing about it. Data that is
/// sorted by one key is not sorted by another.
fn answer(need: &Condition, fact: &Fact) -> Option<bool> {
    match (need, &fact.condition) {
        _ if *need == fact.condition => Some(fact.holds),
        (Condition::Sorted(_), Condition::Sorted(_)) if fact.holds => Some(false),
        _ => None,
    }
}

fn describe(condition: &Condition) -> String {
    match condition {
        Condition::Sorted(key) => format!("is sorted by {}", key.name()),
        Condition::Indexed => "has an index".to_owned(),
        Condition::SameReference(input) => format!("uses the same reference as input {input}"),
        Condition::PairedWith(input) => format!("belongs to the samples of input {input}"),
        Condition::Other(text) => format!("meets \"{text}\""),
    }
}

impl Evidence {
    fn strength(&self) -> u8 {
        match self {
            Evidence::Checked { .. } => 3,
            Evidence::Guaranteed => 2,
            Evidence::Asserted => 1,
            Evidence::Unknown => 0,
        }
    }

    fn source(&self) -> String {
        match self {
            Evidence::Checked { version } => format!("a check of version {version} shows"),
            Evidence::Guaranteed => "the producing tool guarantees".to_owned(),
            Evidence::Asserted => "the user asserts".to_owned(),
            Evidence::Unknown => "an unknown source says".to_owned(),
        }
    }
}

impl SortKey {
    fn name(self) -> &'static str {
        match self {
            SortKey::Coordinate => "coordinate",
            SortKey::Name => "name",
        }
    }
}

fn holds() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fact(condition: Condition, holds: bool, evidence: Evidence) -> Fact {
        Fact {
            condition,
            holds,
            evidence,
        }
    }

    #[test]
    fn checked_beats_asserted() {
        let need = [Condition::Indexed];
        let facts = [
            fact(Condition::Indexed, true, Evidence::Asserted),
            fact(
                Condition::Indexed,
                false,
                Evidence::Checked {
                    version: "v2".to_owned(),
                },
            ),
        ];
        let findings = check(&need, &facts);
        assert_eq!(findings[0].fit, Fit::DoesNotFit, "{findings:?}");
        assert!(findings[0].reason.contains("version v2"), "{findings:?}");
    }

    #[test]
    fn unknown_evidence_warns() {
        for holds in [true, false] {
            let facts = [fact(Condition::Indexed, holds, Evidence::Unknown)];
            let findings = check(&[Condition::Indexed], &facts);
            assert_eq!(findings[0].fit, Fit::Unknown, "holds {holds}: {findings:?}");
            assert_eq!(findings[0].evidence, Some(Evidence::Unknown));
        }
    }

    #[test]
    fn other_sort_refuses() {
        let need = [Condition::Sorted(SortKey::Coordinate)];
        let facts = [fact(
            Condition::Sorted(SortKey::Name),
            true,
            Evidence::Guaranteed,
        )];
        let findings = check(&need, &facts);
        assert_eq!(findings[0].fit, Fit::DoesNotFit);
        assert_eq!(findings[0].evidence, Some(Evidence::Guaranteed));
        assert!(
            findings[0].reason.contains("sorted by name"),
            "{findings:?}"
        );
    }

    #[test]
    fn reads_evidence_json() {
        let fact: Fact = serde_json::from_value(serde_json::json!({
            "condition": {"same_reference": "genome"},
            "evidence": {"checked": {"version": "7"}}
        }))
        .expect("a fact");
        assert!(fact.holds);
        assert_eq!(
            fact.condition,
            Condition::SameReference("genome".to_owned())
        );
    }
}
