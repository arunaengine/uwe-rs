//! Groups the file table of a BIDS dataset into the items of `fromBIDS`, as the config says.
//! The plugin's rules are partly open; the recorded datasets decide where its spec is silent.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::config::{Config, Key, Selector, Set};
use super::files::Row;

/// A part of an item: a file by its row, a list, or a map by name.
#[derive(Clone, Debug)]
pub(super) enum Node {
    File(usize),
    List(Vec<Node>),
    Map(Vec<(String, Node)>),
}

/// One item: the values of the loop entities, and an entry per config key that has files.
#[derive(Clone, Debug)]
pub(super) struct Item {
    pub values: Vec<(String, String)>,
    pub entries: Vec<(String, Node)>,
    /// The subject and session of its files, which cross-modal entries must share.
    origin: (String, String),
}

/// The rows of one group of loop values, by config key.
struct Group {
    id: Vec<String>,
    origin: (String, String),
    rows: Vec<Vec<usize>>,
}

/// The items of the dataset, before cross-modal entries and participant data.
pub(super) fn items(config: &Config, rows: &[Row]) -> Vec<Item> {
    // A mixed set takes the files of its suffix from a sequential set of the same suffix.
    let mixed: Vec<&str> = (config.keys.iter())
        .filter(|key| matches!(key.set, Set::Mixed { .. }))
        .map(|key| key.suffix.as_str())
        .collect();
    // The extensions that a key collects count for every key of its suffix.
    // A suffix without files of those extensions, like `events` with `.tsv`, takes any.
    let extensions = |suffix: &str| -> Vec<&str> {
        let keys = config.keys.iter().filter(|key| key.suffix == suffix);
        let known: Vec<&str> = keys
            .flat_map(|key| key.extensions.iter().map(String::as_str))
            .collect();
        let files = rows.iter().filter(|row| row.suffix == suffix);
        match files
            .clone()
            .any(|row| known.contains(&row.extension.as_str()))
        {
            true => known,
            false => files.map(|row| row.extension.as_str()).collect(),
        }
    };
    let mut groups: Vec<Group> = Vec::new();
    for (index, key) in config.keys.iter().enumerate() {
        let extensions = extensions(&key.suffix);
        if matches!(key.set, Set::Sequential { .. }) && mixed.contains(&key.suffix.as_str()) {
            continue;
        }
        let collapsed = sequence(key);
        for (at, row) in rows.iter().enumerate() {
            if row.suffix != key.suffix
                || !extensions.contains(&row.extension.as_str())
                || !selected(key, row)
            {
                continue;
            }
            let id: Vec<String> = (config.loop_over.iter())
                .map(|column| match collapsed.contains(column) {
                    true => "NA".to_owned(),
                    false => row.entity(column).to_owned(),
                })
                .collect();
            let group = match groups.iter().position(|group| group.id == id) {
                Some(found) => &mut groups[found],
                None => {
                    let origin = (
                        row.entity("subject").to_owned(),
                        row.entity("session").to_owned(),
                    );
                    let rows = vec![Vec::new(); config.keys.len()];
                    groups.push(Group { id, origin, rows });
                    groups.last_mut().expect("a group was added")
                }
            };
            group.rows[index].push(at);
        }
    }
    let mut items = Vec::new();
    for group in &groups {
        items.extend(group_items(config, rows, group));
    }
    cross_modal(config, items)
}

/// The entity columns of a key's sequence, which do not split it into groups.
fn sequence(key: &Key) -> Vec<String> {
    match &key.set {
        Set::Sequential { by, .. } => by.clone(),
        Set::Mixed { sequence, .. } => vec![sequence.clone()],
        _ => Vec::new(),
    }
}

/// Whether a row matches one of the named groups of a key, or any row for other sets.
fn selected(key: &Key, row: &Row) -> bool {
    match &key.set {
        Set::Named(groups) | Set::Mixed { groups, .. } => name_of(groups, row).is_some(),
        _ => true,
    }
}

fn name_of<'a>(groups: &'a [(String, Selector)], row: &Row) -> Option<&'a str> {
    (groups.iter())
        .find(|(_, selector)| {
            selector
                .iter()
                .all(|(column, value)| row.entity(column) == value)
        })
        .map(|(name, _)| name.as_str())
}

/// The key of a file in an entry: `nii` for NIfTI files, else the extension.
fn file_key(row: &Row) -> String {
    match row.extension.as_str() {
        "nii.gz" | "nii" => "nii".into(),
        other => other.into(),
    }
}

/// Whether a later file takes the place of an earlier one: when they differ in an entity
/// that the key does not exclude; files that differ only in excluded ones keep the first.
fn replaces(rows: &[Row], earlier: usize, later: usize, excluded: &[String]) -> bool {
    let kept = |at: usize| -> Vec<&(String, String)> {
        let entities = rows[at].entities.iter();
        entities
            .filter(|(column, _)| !excluded.contains(column))
            .collect()
    };
    kept(earlier) != kept(later)
}

/// A map of the files by file key, sorted, with one file of each key.
fn files(rows: &[Row], indexes: &[usize], excluded: &[String]) -> Node {
    let mut map: Vec<(String, usize)> = Vec::new();
    for at in indexes {
        let key = file_key(&rows[*at]);
        match map.iter_mut().find(|(known, _)| *known == key) {
            Some((_, earlier)) if replaces(rows, *earlier, *at, excluded) => *earlier = *at,
            Some(_) => {}
            None => map.push((key, *at)),
        }
    }
    let mut map: Vec<(String, Node)> = map
        .into_iter()
        .map(|(key, at)| (key, Node::File(at)))
        .collect();
    map.sort_by(|a, b| java_order(&a.0, &b.0));
    Node::Map(map)
}

/// A map of lists of files by file key, each list in the order of the sequence values.
fn lists(
    rows: &[Row],
    indexes: &[usize],
    by: &[String],
    hierarchical: bool,
    excluded: &[String],
) -> Node {
    // One file per sequence value and file key.
    let values =
        |at: &usize| -> Vec<String> { by.iter().map(|c| rows[*at].entity(c).to_owned()).collect() };
    let mut sorted: Vec<usize> = Vec::new();
    for at in indexes {
        let same = |known: &usize| {
            values(known) == values(at) && file_key(&rows[*known]) == file_key(&rows[*at])
        };
        match sorted.iter().position(same) {
            Some(place) if replaces(rows, sorted[place], *at, excluded) => sorted[place] = *at,
            Some(_) => {}
            None => sorted.push(*at),
        }
    }
    sorted.sort_by_key(values);
    let mut keys: Vec<String> = sorted.iter().map(|at| file_key(&rows[*at])).collect();
    keys.sort_by(|a, b| java_order(a, b));
    keys.dedup();
    let map = keys.into_iter().map(|key| {
        let chosen: Vec<usize> = sorted
            .iter()
            .copied()
            .filter(|at| file_key(&rows[*at]) == key)
            .collect();
        let node = match hierarchical && by.len() > 1 {
            true => {
                let mut outer: Vec<(String, Vec<Node>)> = Vec::new();
                for at in chosen {
                    let first = rows[at].entity(&by[0]).to_owned();
                    match outer.iter_mut().find(|(value, _)| *value == first) {
                        Some((_, inner)) => inner.push(Node::File(at)),
                        None => outer.push((first, vec![Node::File(at)])),
                    }
                }
                Node::List(
                    outer
                        .into_iter()
                        .map(|(_, inner)| Node::List(inner))
                        .collect(),
                )
            }
            false => Node::List(chosen.into_iter().map(Node::File).collect()),
        };
        (key, node)
    });
    Node::Map(map.collect())
}

/// The rows of each named group, in the order their first files come.
fn named(
    groups: &[(String, Selector)],
    rows: &[Row],
    indexes: &[usize],
) -> Vec<(String, Vec<usize>)> {
    let mut found: Vec<(String, Vec<usize>)> = Vec::new();
    for at in indexes {
        let Some(name) = name_of(groups, &rows[*at]) else {
            continue;
        };
        match found.iter_mut().find(|(known, _)| known == name) {
            Some((_, members)) => members.push(*at),
            None => found.push((name.to_owned(), vec![*at])),
        }
    }
    found
}

/// The items of one group: the entries of named, sequential and mixed sets, then one item for
/// each combination of the variants of the plain sets.
fn group_items(config: &Config, rows: &[Row], group: &Group) -> Vec<Item> {
    // Entries by kind of set and the place of their first file, then plain sets by name.
    let mut ordered: Vec<(usize, usize, String, Node)> = Vec::new();
    let mut plain: Vec<(String, Vec<Node>)> = Vec::new();
    for (key, indexes) in config.keys.iter().zip(&group.rows) {
        let Some(first) = indexes.first().copied() else {
            continue;
        };
        let complete = |names: &[(String, Vec<usize>)]| {
            (key.required.iter()).all(|required| names.iter().any(|(name, _)| name == required))
        };
        match &key.set {
            Set::Plain => plain.push((key.name.clone(), variants(config, key, rows, indexes))),
            Set::Named(groups) => {
                let found = named(groups, rows, indexes);
                if complete(&found) {
                    let map = found
                        .iter()
                        .map(|(name, members)| (name.clone(), files(rows, members, &key.excluded)));
                    ordered.push((0, first, key.name.clone(), Node::Map(map.collect())));
                }
            }
            Set::Sequential { by, hierarchical } => {
                ordered.push((
                    1,
                    first,
                    key.name.clone(),
                    lists(rows, indexes, by, *hierarchical, &key.excluded),
                ));
            }
            Set::Mixed { sequence, groups } => {
                let found = named(groups, rows, indexes);
                if complete(&found) {
                    let by = std::slice::from_ref(sequence);
                    let map = found.iter().map(|(name, members)| {
                        (name.clone(), lists(rows, members, by, false, &key.excluded))
                    });
                    ordered.push((2, first, key.name.clone(), Node::Map(map.collect())));
                }
            }
        }
    }
    // With a mixed set in the group, a plain set gives only its first variant (recorded).
    if ordered.iter().any(|(kind, _, _, _)| *kind == 2) {
        for (_, choices) in &mut plain {
            choices.truncate(1);
        }
    }
    ordered.sort_by_key(|(kind, first, _, _)| (*kind, *first));
    plain.sort_by(|a, b| java_order(&a.0, &b.0));
    let base: Vec<(String, Node)> = ordered
        .into_iter()
        .map(|(_, _, name, node)| (name, node))
        .collect();
    let mut combinations = vec![base];
    for (name, choices) in plain {
        let mut next = Vec::new();
        for entries in &combinations {
            for choice in &choices {
                let mut entries = entries.clone();
                entries.push((name.clone(), choice.clone()));
                next.push(entries);
            }
        }
        combinations = next;
    }
    let values: Vec<(String, String)> = config
        .loop_over
        .iter()
        .cloned()
        .zip(group.id.iter().cloned())
        .collect();
    (combinations.into_iter())
        .filter(|entries| !entries.is_empty())
        .map(|entries| Item {
            values: values.clone(),
            entries,
            origin: group.origin.clone(),
        })
        .collect()
}

/// The entity values that tell the variants of a plain set apart.
type Signature<'a> = Vec<(&'a str, String)>;

/// The variants of a plain set: its files split by the entities outside the loop that the
/// key does not exclude, in the order their first files come.
fn variants(config: &Config, key: &Key, rows: &[Row], indexes: &[usize]) -> Vec<Node> {
    let mut found: Vec<(Signature, Vec<usize>)> = Vec::new();
    for at in indexes {
        let signature: Signature = (rows[*at].entities.iter())
            .filter(|(column, _)| {
                !config.loop_over.contains(column) && !key.excluded.contains(column)
            })
            .map(|(column, value)| (column.as_str(), value.clone()))
            .collect();
        match found.iter_mut().find(|(known, _)| *known == signature) {
            Some((_, members)) => members.push(*at),
            None => found.push((signature, vec![*at])),
        }
    }
    found
        .iter()
        .map(|(_, members)| files(rows, members, &key.excluded))
        .collect()
}

/// Adds the cross-modal entries, then leaves out the items that only hold such entries.
fn cross_modal(config: &Config, items: Vec<Item>) -> Vec<Item> {
    let sources: Vec<&str> = config
        .keys
        .iter()
        .flat_map(|key| key.cross.iter().map(String::as_str))
        .collect();
    let mut result = Vec::new();
    for item in &items {
        if item
            .entries
            .iter()
            .all(|(name, _)| sources.contains(&name.as_str()))
        {
            continue;
        }
        let mut item = item.clone();
        let own: Vec<String> = item.entries.iter().map(|(name, _)| name.clone()).collect();
        for name in own {
            let Some(key) = config.keys.iter().find(|key| key.name == name) else {
                continue;
            };
            for wanted in &key.cross {
                // The last entry of the subject and session takes the place of the item's own
                // one, as the plugin does, which can leave equal items.
                let found = (items.iter().rev())
                    .filter(|other| other.origin == item.origin)
                    .find_map(|other| other.entries.iter().find(|(known, _)| known == wanted))
                    .cloned();
                let Some(entry) = found else {
                    continue;
                };
                match item.entries.iter_mut().find(|(known, _)| known == wanted) {
                    Some(own) => *own = entry,
                    None => item.entries.push(entry),
                }
            }
        }
        result.push(item);
    }
    result
}

/// Java's order of strings, by UTF-16 code units.
fn java_order(a: &str, b: &str) -> std::cmp::Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}
