//! A merge of value trees by key, like the merge patches of Kubernetes, as Argo joins a
//! Workflow with the WorkflowTemplate of its `workflowTemplateRef` and merges
//! `templateDefaults` into templates: mappings merge key by key, lists of named items merge by
//! name, and anything else in the overlay wins.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use argo_parser::{Entry, Node, Value};

/// `overlay` merged onto `base`. A null in the overlay keeps the base value, as Go's typed
/// structs leave empty fields out of the patch.
pub fn merge(base: &Node, overlay: &Node) -> Node {
    match (&base.value, &overlay.value) {
        (_, Value::Null) => base.clone(),
        (Value::Mapping(base_entries), Value::Mapping(overlay_entries)) => {
            let mut entries: Vec<Entry> = base_entries.clone();
            for entry in overlay_entries {
                match entries
                    .iter_mut()
                    .find(|existing| existing.key == entry.key)
                {
                    Some(existing) => existing.value = merge(&existing.value, &entry.value),
                    None => entries.push(entry.clone()),
                }
            }
            Node::new(Value::Mapping(entries), overlay.position)
        }
        (Value::Sequence(base_items), Value::Sequence(overlay_items)) => {
            match merge_key(base_items, overlay_items) {
                Some(key) => {
                    let mut items: Vec<Node> = base_items.clone();
                    for item in overlay_items {
                        let name = item.get(key).and_then(Node::as_str);
                        match items
                            .iter_mut()
                            .find(|existing| existing.get(key).and_then(Node::as_str) == name)
                        {
                            Some(existing) => *existing = merge(existing, item),
                            None => items.push(item.clone()),
                        }
                    }
                    Node::new(Value::Sequence(items), overlay.position)
                }
                None => overlay.clone(),
            }
        }
        _ => overlay.clone(),
    }
}

/// The key that list items merge by: `name`, or `mountPath` for volume mounts, when every item
/// of both lists has it.
fn merge_key(base: &[Node], overlay: &[Node]) -> Option<&'static str> {
    ["name", "mountPath"].into_iter().find(|key| {
        base.iter()
            .chain(overlay)
            .all(|item| item.get(key).and_then(Node::as_str).is_some())
    })
}

/// The kinds of template, of which a template keeps only its own after defaults are merged.
pub const TEMPLATE_TYPES: [&str; 10] = [
    "container",
    "containerSet",
    "script",
    "resource",
    "dag",
    "steps",
    "suspend",
    "http",
    "plugin",
    "data",
];

/// Merges `templateDefaults` into a template, keeping the template's own type, as Argo's
/// `SetType` does after the merge.
pub fn with_defaults(template: &Node, defaults: &Node) -> Node {
    let own: Vec<&str> = TEMPLATE_TYPES
        .into_iter()
        .filter(|kind| template.get(kind).is_some())
        .collect();
    let mut merged = merge(defaults, template);
    if let Value::Mapping(entries) = &mut merged.value {
        entries.retain(|entry| {
            !TEMPLATE_TYPES.contains(&entry.key.as_str()) || own.contains(&entry.key.as_str())
        });
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use argo_parser::parse_yaml;

    fn node(text: &str) -> Node {
        parse_yaml(text).expect("YAML").expect("a document")
    }

    #[test]
    fn merges_named_collections() {
        let base = node(
            "entrypoint: a\narguments:\n  parameters: [{name: p, value: '1'}, {name: q, value: '2'}]\ntemplates: [{name: a, container: {image: i}}]\n",
        );
        let overlay = node(
            "arguments:\n  parameters: [{name: q, value: '3'}, {name: r}]\ntemplates: [{name: b, steps: []}]\n",
        );
        let merged = merge(&base, &overlay);
        assert_eq!(merged.get("entrypoint").and_then(Node::as_str), Some("a"));
        let Value::Sequence(parameters) = &merged
            .get("arguments")
            .and_then(|arguments| arguments.get("parameters"))
            .expect("parameters")
            .value
        else {
            panic!("a list");
        };
        let values: Vec<Option<&str>> = parameters
            .iter()
            .map(|parameter| parameter.get("value").and_then(Node::as_str))
            .collect();
        assert_eq!(values, [Some("1"), Some("3"), None]);
        let Value::Sequence(templates) = &merged.get("templates").expect("templates").value else {
            panic!("a list");
        };
        assert_eq!(templates.len(), 2);
    }

    #[test]
    fn preserves_template_types() {
        let defaults = node("timeout: 1m\ncontainer: {image: d, env: [{name: A, value: a}]}\n");
        let script = with_defaults(&node("name: s\nscript: {image: s, source: x}\n"), &defaults);
        assert!(script.get("container").is_none());
        assert!(script.get("timeout").is_some());
        let container = with_defaults(&node("name: c\ncontainer: {command: [x]}\n"), &defaults);
        let image = container
            .get("container")
            .and_then(|container| container.get("image"))
            .and_then(Node::as_str);
        assert_eq!(image, Some("d"));
    }
}
