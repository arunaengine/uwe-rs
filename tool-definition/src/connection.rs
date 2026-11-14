//! The check of one connection from an output to an input: kind, cardinality, format and data.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::conditions::{self, Evidence, Fact};
use crate::definition::{Cardinality, Definition, InputPort, OutputFormat, PortKind, ValueType};
use crate::edam::{hierarchy, term_id};
use crate::finding::{Aspect, Finding, Fit, worst};

/// What a connection delivers to an input: an output with its format known, or a workflow
/// input. Build it from a tool output with [`Supply::output`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Supply {
    pub kind: PortKind,
    #[serde(default)]
    pub shape: Shape,
    /// EDAM formats, one of which the data has; empty when not known.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub formats: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub facts: Vec<Fact>,
}

/// The cardinality of the data, and whether it comes as a list with one such item per sample,
/// as the outputs of a tool that maps over a list do.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Shape {
    pub cardinality: Cardinality,
    #[serde(default)]
    pub per_sample: bool,
}

/// How well the formats fit, from best to worst.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FormatMatch {
    /// The port takes the exact term.
    Exact,
    /// The port takes a more general term, or every format.
    General,
    /// Not known: no format, a term outside EDAM, or a more general given term.
    Unknown,
    Mismatch,
}

/// The answer for one connection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Check {
    pub fit: Fit,
    /// The tool maps over the list: it runs once per item, and each of its outputs becomes a
    /// list that keeps the sample id of its item.
    pub scatter: bool,
    pub format: FormatMatch,
    /// Every needed data condition, and each other answer that is not a plain fit.
    pub findings: Vec<Finding>,
}

/// Checks what `supply` delivers against what `port` takes.
pub fn check(supply: &Supply, port: &InputPort) -> Check {
    let mut findings = Vec::new();
    if let Some(reason) = kind_misfit(supply.kind, port.kind) {
        findings.push(Finding::new(Aspect::Kind, Fit::DoesNotFit, reason));
    }
    let (fit, flow, reason) = cardinality(supply.shape, port.cardinality);
    if let Some(reason) = reason {
        findings.push(Finding::new(Aspect::Cardinality, fit, reason));
    }
    let (format, reason) = format_match(&supply.formats, &port.formats);
    if let Some(reason) = reason {
        let fit = match format {
            FormatMatch::Mismatch => Fit::DoesNotFit,
            _ => Fit::Unknown,
        };
        findings.push(Finding::new(Aspect::Format, fit, reason));
    }
    findings.extend(conditions::check(&port.conditions, &supply.facts));
    Check {
        fit: worst(&findings),
        scatter: flow == Flow::Scatter,
        format,
        findings,
    }
}

impl Supply {
    /// The supply of output `name` of `definition`; `None` when it has no such output. A format
    /// that follows an input takes the formats of `inputs`, else the formats that input takes.
    pub fn output(
        definition: &Definition,
        name: &str,
        inputs: &BTreeMap<String, Supply>,
        per_sample: bool,
    ) -> Option<Supply> {
        let output = definition.output(name)?;
        let formats = match &output.format {
            OutputFormat::Terms(terms) => terms.clone(),
            OutputFormat::Follows(input) => match inputs.get(input) {
                Some(supply) if !supply.formats.is_empty() => supply.formats.clone(),
                _ => definition
                    .input(input)
                    .map(|port| port.formats.clone())
                    .unwrap_or_default(),
            },
        };
        let facts = output
            .guarantees
            .iter()
            .map(|condition| Fact {
                condition: condition.clone(),
                holds: true,
                evidence: Evidence::Guaranteed,
            })
            .collect();
        Some(Supply {
            kind: output.kind,
            shape: Shape {
                cardinality: output.cardinality,
                per_sample,
            },
            formats,
            facts,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flow {
    Plain,
    Scatter,
    Gather,
}

fn kind_misfit(given: PortKind, taken: PortKind) -> Option<String> {
    match (given, taken) {
        _ if given == taken => None,
        (PortKind::Value(ValueType::Integer), PortKind::Value(ValueType::Number)) => None,
        _ => Some(format!(
            "the port takes {} and gets {}",
            kind_name(taken),
            kind_name(given)
        )),
    }
}

fn kind_name(kind: PortKind) -> String {
    match kind {
        PortKind::File => "a file".to_owned(),
        PortKind::Directory => "a directory".to_owned(),
        PortKind::Value(value) => {
            let name = serde_json::to_value(value).unwrap_or_default();
            format!("a value of type {}", name.as_str().unwrap_or_default())
        }
    }
}

/// How the data flows into a port of cardinality `taken`, with a reason when it is not plain.
fn cardinality(given: Shape, taken: Cardinality) -> (Fit, Flow, Option<String>) {
    use Cardinality::{List, One, Pair};
    const PAIR: &str = "a pair is two files of one sample";
    match (given.per_sample, given.cardinality, taken) {
        (false, item, port) if item == port => (Fit::Fits, Flow::Plain, None),
        (false, One | Pair, List) => (Fit::Fits, Flow::Plain, None),
        (false, List, One) => (
            Fit::Fits,
            Flow::Scatter,
            Some(
                "the tool maps over the list, once per item; its outputs become lists that \
                 keep the sample id of each item"
                    .to_owned(),
            ),
        ),
        (true, item, port) if item == port || (item, port) == (Pair, List) => (
            Fit::Fits,
            Flow::Scatter,
            Some(
                "the tool maps over the samples, once per sample; its outputs become lists \
                 that keep the sample id of each item"
                    .to_owned(),
            ),
        ),
        (true, One, List) => (
            Fit::Fits,
            Flow::Gather,
            Some(
                "the port takes the items of all samples as one list; each item keeps its \
                 sample id"
                    .to_owned(),
            ),
        ),
        (true, List, One) => (
            Fit::Unknown,
            Flow::Plain,
            Some("a list per sample would map twice; nested lists are not checked".to_owned()),
        ),
        (_, _, Pair) => (
            Fit::DoesNotFit,
            Flow::Plain,
            Some(format!(
                "the port takes a pair and gets something else; {PAIR}"
            )),
        ),
        (_, _, _) => (
            Fit::DoesNotFit,
            Flow::Plain,
            Some(format!("the port takes one file and gets a pair; {PAIR}")),
        ),
    }
}

/// How well one of `given` fits any of `taken`, with a reason when it is not a plain fit.
fn format_match(given: &[String], taken: &[String]) -> (FormatMatch, Option<String>) {
    if taken.is_empty() {
        return (FormatMatch::General, None);
    }
    let wanted = describe(taken);
    if given.is_empty() {
        let reason = format!("the data has no declared format; the port takes {wanted}");
        return (FormatMatch::Unknown, Some(reason));
    }
    let best: Vec<FormatMatch> = given
        .iter()
        .map(|term| {
            taken
                .iter()
                .map(|port| pair_match(term, port))
                .min()
                .unwrap_or(FormatMatch::Mismatch)
        })
        .collect();
    let all = |wanted: FormatMatch| best.iter().all(|found| *found <= wanted);
    let found = describe(given);
    let edam = hierarchy();
    let unknown = given.iter().chain(taken).find(|term| !edam.contains(term));
    if all(FormatMatch::Exact) {
        (FormatMatch::Exact, None)
    } else if all(FormatMatch::General) {
        (FormatMatch::General, None)
    } else if best.iter().all(|found| *found == FormatMatch::Mismatch) {
        let reason = format!(
            "the data is {found} and the port takes {wanted}; neither is a kind of the other"
        );
        (FormatMatch::Mismatch, Some(reason))
    } else if let Some(term) = unknown {
        let reason = format!(
            "{} is not a term of EDAM {}; the format is not checked",
            term_id(term),
            edam.release()
        );
        (FormatMatch::Unknown, Some(reason))
    } else if best.contains(&FormatMatch::Unknown) {
        let reason =
            format!("the data is {found}, more general than {wanted}; the data may still fit");
        (FormatMatch::Unknown, Some(reason))
    } else {
        let reason = format!("the data is {found}; only some of these fit {wanted}");
        (FormatMatch::Unknown, Some(reason))
    }
}

/// Equal or more general taken terms fit, unknown terms give unknown. Each FASTQ variant fits
/// generic FASTQ; generic FASTQ into a variant is unknown, the variants stay apart.
fn pair_match(given: &str, taken: &str) -> FormatMatch {
    const FASTQ: &str = "format_1930";
    const VARIANTS: [&str; 3] = ["format_1931", "format_1932", "format_1933"];
    let (given, taken) = (term_id(given), term_id(taken));
    let edam = hierarchy();
    if given == taken {
        FormatMatch::Exact
    } else if !edam.contains(given) || !edam.contains(taken) {
        FormatMatch::Unknown
    } else if edam.is_kind(given, taken) || (taken == FASTQ && VARIANTS.contains(&given)) {
        FormatMatch::General
    } else if edam.is_kind(taken, given) || (given == FASTQ && VARIANTS.contains(&taken)) {
        FormatMatch::Unknown
    } else {
        FormatMatch::Mismatch
    }
}

fn describe(terms: &[String]) -> String {
    let edam = hierarchy();
    let names: Vec<String> = terms.iter().map(|term| edam.describe(term)).collect();
    names.join(" or ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fastq_variants() {
        assert_eq!(
            pair_match("format_1931", "format_1930"),
            FormatMatch::General
        );
        assert_eq!(
            pair_match("format_1930", "format_1932"),
            FormatMatch::Unknown
        );
        let (found, reason) =
            format_match(&["format_1930".to_owned()], &["format_1933".to_owned()]);
        assert_eq!(found, FormatMatch::Unknown);
        assert!(reason.is_some_and(|reason| reason.contains("may still fit")));
        assert_eq!(
            pair_match("format_1931", "format_1932"),
            FormatMatch::Mismatch
        );
    }

    #[test]
    fn integer_fits_number() {
        let integer = PortKind::Value(ValueType::Integer);
        let number = PortKind::Value(ValueType::Number);
        assert_eq!(kind_misfit(integer, number), None);
        let reason = kind_misfit(number, integer).expect("a misfit");
        assert_eq!(
            reason,
            "the port takes a value of type integer and gets a value of type number"
        );
        assert!(kind_misfit(PortKind::File, PortKind::Directory).is_some());
    }

    #[test]
    fn pair_into_list() {
        let pair = Shape {
            cardinality: Cardinality::Pair,
            per_sample: false,
        };
        assert_eq!(cardinality(pair, Cardinality::List).1, Flow::Plain);
        assert_eq!(cardinality(pair, Cardinality::One).0, Fit::DoesNotFit);
        let samples = Shape {
            per_sample: true,
            ..pair
        };
        assert_eq!(cardinality(samples, Cardinality::List).1, Flow::Scatter);
        assert_eq!(cardinality(samples, Cardinality::Pair).1, Flow::Scatter);
    }
}
