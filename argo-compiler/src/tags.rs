//! Finds the tags that a step or DAG task reads in its texts, like `steps.make.outputs.result`.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeSet;
use std::sync::OnceLock;

use argo_parser::{Node, Value as Yaml};
use regex::Regex;

/// A tag that a step reads and that wir can give it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tag {
    /// `inputs.parameters.<name>` or `inputs.artifacts.<name>` of the holding template.
    Input(String),
    /// What a step or task made: the prefix `steps.<name>` or `tasks.<name>`, and the rest,
    /// like `outputs.result`.
    Made(String, String),
    /// The item of a loop.
    Item,
    /// `workflow.status` or `workflow.failures`, which an exit handler reads.
    Status(String),
}

impl Tag {
    /// The tag as the step reads it.
    pub fn name(&self) -> String {
        match self {
            Tag::Input(name) | Tag::Status(name) => name.clone(),
            Tag::Made(prefix, rest) => format!("{prefix}.{rest}"),
            Tag::Item => "item".to_owned(),
        }
    }
}

fn pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        let name = "[A-Za-z0-9_-]+";
        let text = format!(
            r"\b(?:(?P<made>(?:steps|tasks)\.{name})\.(?P<rest>outputs\.(?:parameters|artifacts)\.{name}|outputs\.{name}|{name})|(?P<input>inputs\.(?:parameters|artifacts)\.{name})|(?P<status>workflow\.(?:status|failures))|(?P<item>item)\b)|(?P<bracket>(?:steps|tasks|inputs)\[)"
        );
        Regex::new(&text).unwrap_or_else(|_| unreachable!("the pattern is valid"))
    })
}

/// The tags in the `{{...}}` parts of the texts of a step or task, or why one cannot be read
/// from wir values, like the status of a step.
pub fn read_tags(step: &Node) -> Result<BTreeSet<Tag>, String> {
    let mut texts = Vec::new();
    collect_texts(step, &mut texts);
    let mut tags = BTreeSet::new();
    for text in texts {
        let mut rest = text.as_str();
        while let Some(start) = rest.find("{{") {
            let after = &rest[start + 2..];
            let end = after.find("}}").unwrap_or(after.len());
            scan(&after[..end], &mut tags)?;
            rest = &after[end.min(after.len())..];
        }
    }
    Ok(tags)
}

fn scan(tag: &str, tags: &mut BTreeSet<Tag>) -> Result<(), String> {
    for found in pattern().captures_iter(tag) {
        if found.name("bracket").is_some() {
            return Err(format!("the tag {{{{{tag}}}}} reads a step by an index"));
        }
        if let (Some(made), Some(rest)) = (found.name("made"), found.name("rest")) {
            let rest = rest.as_str();
            let known = rest.starts_with("outputs.parameters.")
                || rest.starts_with("outputs.artifacts.")
                || rest == "outputs.result"
                || rest == "exitCode";
            if !known {
                return Err(format!(
                    "the tag {{{{{tag}}}}} reads {rest} of {}, which wir has no value for",
                    made.as_str()
                ));
            }
            tags.insert(Tag::Made(made.as_str().to_owned(), rest.to_owned()));
        } else if let Some(input) = found.name("input") {
            tags.insert(Tag::Input(input.as_str().to_owned()));
        } else if let Some(status) = found.name("status") {
            tags.insert(Tag::Status(status.as_str().to_owned()));
        } else if found.name("item").is_some() {
            tags.insert(Tag::Item);
        }
    }
    Ok(())
}

/// The texts of a step, apart from the hooks, which run as steps of their own.
fn collect_texts(node: &Node, texts: &mut Vec<String>) {
    match &node.value {
        Yaml::String(text) => texts.push(text.clone()),
        Yaml::Sequence(items) => items.iter().for_each(|item| collect_texts(item, texts)),
        Yaml::Mapping(entries) => (entries.iter())
            .filter(|entry| entry.key != "hooks")
            .for_each(|entry| collect_texts(&entry.value, texts)),
        _ => {}
    }
}

/// Whether a text holds a tag.
pub fn has_tag(text: &str) -> bool {
    text.contains("{{")
}

/// The one tag that a text is, like `{{steps.make.outputs.result}}`, with spaces allowed.
pub fn single_tag(text: &str) -> Option<&str> {
    let inner = text.trim().strip_prefix("{{")?.strip_suffix("}}")?.trim();
    (!inner.contains("{{") && !inner.contains("}}") && !inner.starts_with('=')).then_some(inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_read_tags() {
        let node = argo_parser::parse_yaml(
            "name: a\narguments:\n  parameters:\n  - {name: x, value: '{{steps.b.outputs.parameters.y}}-{{item.k}}'}\n  - {name: z, value: '{{=asInt(inputs.parameters.n) + 1}}'}\nwhen: '{{tasks.c.outputs.result}} == 1'\nhooks: {exit: {template: '{{steps.d.status}}'}}\n",
        )
        .unwrap()
        .unwrap();
        let tags = read_tags(&node).unwrap();
        let names: Vec<String> = tags.iter().map(Tag::name).collect();
        assert_eq!(
            names,
            [
                "inputs.parameters.n",
                "steps.b.outputs.parameters.y",
                "tasks.c.outputs.result",
                "item"
            ]
        );
        let status = argo_parser::parse_yaml("when: '{{steps.d.status}} == Failed'\n")
            .unwrap()
            .unwrap();
        assert!(read_tags(&status).unwrap_err().contains("status"));
        assert_eq!(
            single_tag(" {{ steps.a.outputs.result }} "),
            Some("steps.a.outputs.result")
        );
        assert_eq!(single_tag("{{a}}-{{b}}"), None);
    }
}
