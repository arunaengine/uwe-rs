//! The entries of a step group or a DAG task, and the cursors that make loop entries from items.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use argo_parser::Node;

use crate::items::{item_variables, substitute_item, unknown_item};
use crate::node::ArtifactValue;
use crate::run::{Expanded, Run};
use crate::tags::Variables;
use crate::value::Value;

/// Makes the entry of one item from the step or task with the item substituted.
pub(crate) type Build = fn(String, Node, &str, Vec<ArtifactValue>) -> Expanded;

/// The items of a loop that are not entries yet.
#[derive(Debug)]
pub(crate) struct Cursor {
    /// The step or task name, which names the entries.
    pub name: String,
    /// The position of the step in its group; zero for a DAG task.
    pub slot: usize,
    /// The step or task with the scope substituted, before the item is.
    pub node: Node,
    pub template: String,
    pub artifacts: Vec<ArtifactValue>,
    pub variables: Variables,
    pub items: Vec<Value>,
    pub strict: bool,
    pub next: usize,
    pub build: Build,
}

/// The entries made so far, in the order they were made, with the node of each once it exists.
#[derive(Debug, Default)]
pub(crate) struct Entries {
    pub list: Vec<Rc<Expanded>>,
    /// The position of the step of each entry in its group; zero in a DAG.
    pub slots: Vec<usize>,
    pub nodes: Vec<Option<usize>>,
    /// Whether the node of each entry runs its template, rather than ending at once.
    pub runs: Vec<bool>,
    /// The entries whose node, or hooks, may still change.
    pub open: Vec<usize>,
}

impl Entries {
    pub fn push(&mut self, slot: usize, entry: Expanded) {
        self.open.push(self.list.len());
        self.list.push(Rc::new(entry));
        self.slots.push(slot);
        self.nodes.push(None);
        self.runs.push(false);
    }
}

impl Run {
    /// The entry of the next item of a loop, or the error of an item that cannot be one, which
    /// ends the loop.
    pub(crate) fn next_entry(
        &mut self,
        cursor: &mut Cursor,
    ) -> Option<std::result::Result<Expanded, String>> {
        let position = cursor.next;
        let item = cursor.items.get(position)?;
        cursor.next += 1;
        let (values, item_name) = match item_variables(&cursor.name, position, item) {
            Ok(found) => found,
            Err(message) => {
                cursor.next = cursor.items.len();
                return Some(Err(message));
            }
        };
        let strict = if self.options.assume_inputs && unknown_item(item) {
            self.warn(format!(
                "an item of the loop {item_name} is empty; tags that read its fields stay"
            ));
            false
        } else {
            cursor.strict
        };
        let variables = cursor.variables.clone().with_values(values);
        let entry = match substitute_item(&cursor.node, &variables, strict) {
            Ok(node) => (cursor.build)(item_name, node, &cursor.template, cursor.artifacts.clone()),
            Err(message) => Expanded::error(item_name, message),
        };
        Some(Ok(entry))
    }
}
