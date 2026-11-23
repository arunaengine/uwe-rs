//! The EDAM class hierarchy of the embedded release: the label and the parents of each term.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::sync::OnceLock;

/// Ids, labels and parents of the data, format, operation and topic branches; see LICENSE-EDAM.
const DATA: &str = include_str!("../edam.tsv");

/// The location of EDAM as an ontology of formats; runs match its terms by the embedded release
/// instead of reading the file.
pub const ONTOLOGY: &str = "http://edamontology.org/EDAM.owl";

/// The classes of one EDAM release, read once from the embedded data.
#[derive(Debug)]
pub struct Hierarchy {
    release: &'static str,
    terms: HashMap<&'static str, Term>,
}

#[derive(Debug)]
struct Term {
    label: &'static str,
    parents: Vec<&'static str>,
}

/// The hierarchy of the embedded EDAM release.
pub fn hierarchy() -> &'static Hierarchy {
    static HIERARCHY: OnceLock<Hierarchy> = OnceLock::new();
    HIERARCHY.get_or_init(|| Hierarchy::parse(DATA))
}

/// The short id of a term, like `format_2572`, from an IRI, `edam:format_2572` or the id.
pub fn term_id(text: &str) -> &str {
    let text = text.trim();
    let text = text
        .strip_prefix("http://edamontology.org/")
        .or_else(|| text.strip_prefix("https://edamontology.org/"))
        .unwrap_or(text);
    text.strip_prefix("edam:").unwrap_or(text)
}

impl Hierarchy {
    fn parse(text: &'static str) -> Hierarchy {
        let mut release = "";
        let mut terms = HashMap::new();
        for line in text.lines() {
            if let Some(comment) = line.strip_prefix("# EDAM ") {
                release = comment.split(' ').next().unwrap_or_default();
                continue;
            }
            let mut columns = line.split('\t');
            let (Some(id), Some(label)) = (columns.next(), columns.next()) else {
                continue;
            };
            if id.starts_with('#') {
                continue;
            }
            let parents = columns
                .next()
                .unwrap_or_default()
                .split_whitespace()
                .collect();
            terms.insert(id, Term { label, parents });
        }
        Hierarchy { release, terms }
    }

    /// The ids of all terms, in no order.
    pub fn terms(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.terms.keys().copied()
    }

    /// The release tag, like `1.25.20260626T1230Z`.
    pub fn release(&self) -> &str {
        self.release
    }

    /// The number of terms.
    pub fn len(&self) -> usize {
        self.terms.len()
    }

    /// Whether the release has no terms.
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    /// Whether `term` is a class of the release that is not deprecated.
    pub fn contains(&self, term: &str) -> bool {
        self.terms.contains_key(term_id(term))
    }

    /// The preferred label of `term`, like `BAM`.
    pub fn label(&self, term: &str) -> Option<&str> {
        self.terms.get(term_id(term)).map(|term| term.label)
    }

    /// The direct parents of `term`; empty for a root or an unknown term.
    pub fn parents(&self, term: &str) -> &[&str] {
        self.terms
            .get(term_id(term))
            .map_or(&[], |term| term.parents.as_slice())
    }

    /// Whether `term` is `ancestor` or a subclass of it, over all parents.
    pub fn is_kind(&self, term: &str, ancestor: &str) -> bool {
        let ancestor = term_id(ancestor);
        let mut todo = vec![term_id(term)];
        let mut seen = Vec::new();
        while let Some(at) = todo.pop() {
            if at == ancestor {
                return true;
            }
            if seen.contains(&at) {
                continue;
            }
            seen.push(at);
            todo.extend(self.parents(at));
        }
        false
    }

    /// `term` with its label, like `BAM (format_2572)`, or the id alone when unknown.
    pub fn describe(&self, term: &str) -> String {
        let id = term_id(term);
        match self.label(id) {
            Some(label) => format!("{label} ({id})"),
            None => id.to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_release() {
        let edam = hierarchy();
        assert_eq!(edam.release(), "1.25.20260626T1230Z");
        assert!(edam.len() > 2000, "{} terms", edam.len());
        assert_eq!(edam.label("format_2572"), Some("BAM"));
        assert_eq!(
            edam.parents("format_2572"),
            ["format_2057", "format_2333", "format_2920"]
        );
    }

    #[test]
    fn walks_parents() {
        let edam = hierarchy();
        assert!(edam.is_kind("format_2572", "format_1915"));
        assert!(edam.is_kind("http://edamontology.org/format_1930", "edam:format_2182"));
        assert!(!edam.is_kind("format_1929", "format_2182"));
        assert!(edam.is_kind("operation_3198", "operation_0004"));
        assert!(!edam.is_kind("format_9999", "format_1915"));
    }

    #[test]
    fn normalizes_ids() {
        assert_eq!(
            term_id(" https://edamontology.org/topic_0003"),
            "topic_0003"
        );
        assert_eq!(term_id("edam:data_0006"), "data_0006");
        assert_eq!(hierarchy().describe("format_9999"), "format_9999");
        assert_eq!(
            hierarchy().describe("edam:format_2573"),
            "SAM (format_2573)"
        );
    }
}
