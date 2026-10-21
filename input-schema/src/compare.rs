//! Compares schema declarations with the inputs read by workflow code.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeSet;

use serde_json::Value as Json;

use crate::properties::properties;

/// The warnings where the inputs at the top of the schema and the inputs that the code reads
/// differ, at most one of each kind, with the names in order.
pub fn compare(schema: &Json, read: &BTreeSet<String>) -> Vec<String> {
    let declared: BTreeSet<&str> = properties(schema)
        .iter()
        .map(|property| property.name)
        .collect();
    let undeclared: Vec<&str> = read
        .iter()
        .map(String::as_str)
        .filter(|name| !declared.contains(name))
        .collect();
    let unread: Vec<&str> = declared
        .iter()
        .copied()
        .filter(|name| !read.contains(*name))
        .collect();
    let mut warnings = Vec::new();
    if !undeclared.is_empty() {
        warnings.push(format!(
            "the workflow code reads {} that the schema does not declare: {}",
            inputs(undeclared.len()),
            names(&undeclared)
        ));
    }
    if !unread.is_empty() {
        warnings.push(format!(
            "the schema declares {} that the workflow code never reads: {}",
            inputs(unread.len()),
            names(&unread)
        ));
    }
    warnings
}

fn inputs(count: usize) -> &'static str {
    match count {
        1 => "an input",
        _ => "inputs",
    }
}

fn names(names: &[&str]) -> String {
    let quoted: Vec<String> = names.iter().map(|name| format!("`{name}`")).collect();
    quoted.join(", ")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn both_directions() {
        let schema = json!({"properties": {"input": {}, "outdir": {}, "unused": {}}});
        let read: BTreeSet<String> = ["input", "outdir", "typo", "other"]
            .map(str::to_owned)
            .into();
        assert_eq!(
            compare(&schema, &read),
            [
                "the workflow code reads inputs that the schema does not declare: `other`, `typo`",
                "the schema declares an input that the workflow code never reads: `unused`"
            ]
        );
        let all: BTreeSet<String> = ["input", "outdir", "unused"].map(str::to_owned).into();
        assert!(compare(&schema, &all).is_empty());
    }
}
