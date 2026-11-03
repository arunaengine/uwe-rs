//! The entries of a step group or a DAG task, and the cursors that make loop entries from items.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::rc::Rc;

use argo_parser::Node;

use crate::items::{item_variables, substitute_item, unknown_item};
use crate::node::ArtifactValue;
use crate::run::{Expanded, Run};
use crate::tags::Variables;
use crate::value::Value;

/// A loop with more items than this makes its entries as earlier ones finish: at most this
/// many of its entries are unfinished, and none is made while the run has this many tasks out.
pub(crate) const WINDOW: usize = 1024;

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
    /// The loops with more than [`WINDOW`] items that have items left.
    pub cursors: Vec<Cursor>,
    /// How many entries were made when the group started; later ones come from cursors.
    pub eager: usize,
    /// The workflow globals when the group started, which the later entries read.
    pub globals: BTreeMap<String, String>,
    /// The nodes made so far for each step of a group with cursors, to keep declared order.
    pub made: Vec<usize>,
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

/// Whether a loop makes its entries as earlier ones finish. A boundary with `failFast` makes
/// them all at once, as its failure must not stop items that started together.
pub(crate) fn deferred(items: &[Value], fail_fast: bool) -> bool {
    items.len() > WINDOW && !fail_fast
}

impl Run {
    /// Makes entries from the cursors of a group while they have room; `busy` counts the
    /// unfinished entries of each step. Says whether it made any.
    pub(crate) fn make_entries(&mut self, entries: &mut Entries, busy: &[usize]) -> bool {
        let mut room = WINDOW.saturating_sub(self.tasks.len());
        let mut cursors = std::mem::take(&mut entries.cursors);
        let before = entries.list.len();
        for cursor in &mut cursors {
            let busy = busy.get(cursor.slot).copied().unwrap_or_default();
            let mut left = WINDOW.saturating_sub(busy);
            while room > 0
                && left > 0
                && let Some(entry) = self.next_entry(cursor)
            {
                let name = cursor.name.clone();
                entries.push(
                    cursor.slot,
                    entry.unwrap_or_else(|message| Expanded::error(name, message)),
                );
                room -= 1;
                left -= 1;
            }
        }
        cursors.retain(|cursor| cursor.next < cursor.items.len());
        entries.cursors = cursors;
        entries.list.len() > before
    }

    /// Runs `step` with the globals of `snapshot`, so that an entry made late reads what its
    /// loop read when it started; the globals that `step` sets stay.
    pub(crate) fn with_globals<T>(
        &mut self,
        snapshot: &BTreeMap<String, String>,
        step: impl FnOnce(&mut Run) -> T,
    ) -> T {
        let live = std::mem::replace(&mut self.globals, snapshot.clone());
        let result = step(self);
        let seen = std::mem::replace(&mut self.globals, live);
        for (key, value) in seen {
            if snapshot.get(&key) != Some(&value) {
                self.globals.insert(key, value);
            }
        }
        result
    }

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
