//! nf-bids 0.3.0: `channel.fromBIDS` reads a BIDS dataset into items, plus three key operators.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod config;
mod files;
mod groups;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::{Error, parse_json};
use nextflow_evaluator::value::{Key, Native, Object, PathValue, State as ObjectState, Value};

use crate::Context;
use crate::catalogue::{Kind, Plugin, State};
use crate::nf_schema::{entries, get};
use files::Row;
use groups::{Item, Node};

/// `operators.nf` gives the operators over the native helpers of `call`.
pub(crate) const PLUGIN: Plugin = Plugin {
    functions: &["fromBIDS", "groupTupleBy", "joinBy", "combineBy"],
    call: Some(call),
    script: Some(include_str!("operators.nf")),
    factories: &["fromBIDS"],
    ..Plugin::new("nf-bids", Kind::Functions, State::Implemented)
};

fn call(name: &str, arguments: Vec<Value>, context: &mut Context) -> Result<Value, Error> {
    match (name, arguments.as_slice()) {
        ("fromBIDS", [dataset, config]) => from_bids(dataset, config, &Value::Null, context),
        ("fromBIDS", [dataset, config, options]) => from_bids(dataset, config, options, context),
        ("bidsKey", [Value::List(list)]) => Ok(CompositeKey(list.borrow().items.clone()).value()),
        ("bidsKey", [value]) => Ok(value.clone()),
        ("bidsGroups", [pairs, size, remainder]) => {
            let size = size.to_string().parse::<usize>().unwrap_or(0);
            Ok(grouped(&items(pairs), size, remainder.truth()))
        }
        ("bidsPairs", [left, right, remainder]) => {
            Ok(paired(&items(left), &items(right), remainder.truth()))
        }
        _ => Err(Error::Unsupported {
            message: format!("the function `{name}` of the plugin `nf-bids` with these arguments"),
            span: Default::default(),
        }),
    }
}

fn failed(message: impl Into<String>) -> Error {
    Error::Thrown {
        exception: Rc::new(Exception::illegal_argument(message)),
        span: Default::default(),
    }
}

/// The characters that the plugin refuses in paths, which it hands to a shell.
const DANGEROUS: &[char] = &[';', '&', '|', '$', '`', '(', ')', '<', '>', '"', '\'', '\\'];

/// The options of `fromBIDS`, with their defaults.
struct Options {
    flatten: bool,
    unpack: bool,
    ignore: bool,
    defaults: bool,
}

fn from_bids(
    dataset: &Value,
    config: &Value,
    options: &Value,
    context: &mut Context,
) -> Result<Value, Error> {
    let (dataset, config) = (dataset.to_string(), config.to_string());
    for given in [&dataset, &config] {
        if given.contains(DANGEROUS) {
            return Err(failed(format!(
                "BIDS directory contains dangerous characters: {given}"
            )));
        }
    }
    let launch = PathBuf::from(context.host.launch_dir());
    let root = launch.join(&dataset);
    if !root.is_dir() {
        return Err(failed(format!(
            "BIDS directory does not exist: {}",
            root.display()
        )));
    }
    let text = std::fs::read_to_string(launch.join(&config))
        .map_err(|_| failed(format!("Configuration file not found: {config}")))?;
    let config = config::read(&text, &config).map_err(failed)?;
    let flag =
        |name: &str, default: bool| get(options, name).map_or(default, |value| value.truth());
    let options = Options {
        flatten: flag("flatten_output", true),
        unpack: flag("unpack_json_sidecar", false),
        ignore: flag("use_bidsignore", true),
        defaults: flag("use_default_ignores", true),
    };
    tracing::info!(
        "[nf-bids] Starting BIDS dataset parsing: {}",
        root.display()
    );
    let rows = files::table(&root, options.ignore, options.defaults);
    let items = groups::items(&config, &rows);
    if items.is_empty() {
        return Err(failed(
            "No data groups were processed! This could indicate:",
        ));
    }
    let participants = participants(&root);
    let output = Output {
        root: &root,
        rows: &rows,
        options: &options,
    };
    let values = items.iter().map(|item| match options.flatten {
        true => output.flat(item, &participants),
        false => output.legacy(item),
    });
    Ok(Value::list(values.collect()))
}

/// What turns items into values.
struct Output<'a> {
    root: &'a Path,
    rows: &'a [Row],
    options: &'a Options,
}

impl Output<'_> {
    /// An item as a map: `meta`, then an entry per config key with files as paths.
    fn flat(&self, item: &Item, participants: &[Vec<(String, String)>]) -> Value {
        let text = |text: &str| Value::string(text);
        let mut meta: Vec<(String, String)> = item.values.clone();
        let subject = item
            .values
            .iter()
            .find(|(column, _)| column == "subject")
            .map(|(_, v)| v.clone());
        let ids = ["participant_id", "participant", "subject_id"];
        let row = participants.iter().find(|row| {
            row.iter().any(|(column, value)| {
                ids.contains(&column.as_str()) && Some(value) == subject.as_ref()
            })
        });
        for (column, value) in row.into_iter().flatten() {
            if !ids.contains(&column.as_str()) && !meta.iter().any(|(known, _)| known == column) {
                meta.push((column.clone(), value.clone()));
            }
        }
        let mut entries = vec![(
            text("meta"),
            Value::map(meta.iter().map(|(k, v)| (text(k), text(v)))),
        )];
        for (name, node) in &item.entries {
            entries.push((text(name), self.value(node, true)));
        }
        Value::map(entries)
    }

    /// An item in the tuple form of `flatten_output: false`, with paths as text.
    fn legacy(&self, item: &Item) -> Value {
        let text = |text: &str| Value::string(text);
        let mut data = Vec::new();
        let mut paths = Vec::new();
        for (name, node) in &item.entries {
            data.push((text(name), self.value(node, false)));
            collect(node, &mut |at| paths.push(Value::string(self.relative(at))));
        }
        let parent = self.root.parent().unwrap_or(Path::new("/"));
        let mut fields = vec![
            (text("data"), Value::map(data)),
            (text("filePaths"), Value::list(paths)),
            (text("bidsParentDir"), text(&parent.to_string_lossy())),
        ];
        fields.extend(item.values.iter().map(|(k, v)| (text(k), text(v))));
        let values = item.values.iter().map(|(_, value)| text(value)).collect();
        Value::list(vec![Value::list(values), Value::map(fields)])
    }

    /// A node: files as paths, as text relative to the dataset's parent, or JSON as maps.
    fn value(&self, node: &Node, paths: bool) -> Value {
        match node {
            Node::File(at) => {
                let row = &self.rows[*at];
                if self.options.unpack && row.extension == "json" {
                    let text = std::fs::read_to_string(&row.path).unwrap_or_default();
                    return parse_json(&text).unwrap_or(Value::Null);
                }
                match paths {
                    true => Value::Path(Rc::new(PathValue::parse(
                        &self.root.join(&row.relative).to_string_lossy(),
                        "/",
                    ))),
                    false => Value::string(self.relative(*at)),
                }
            }
            Node::List(items) => {
                Value::list(items.iter().map(|item| self.value(item, paths)).collect())
            }
            Node::Map(fields) => Value::map(
                fields
                    .iter()
                    .map(|(name, node)| (Value::string(name.as_str()), self.value(node, paths))),
            ),
        }
    }

    fn relative(&self, at: usize) -> String {
        let name = self
            .root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        format!("{name}/{}", self.rows[at].relative)
    }
}

fn collect(node: &Node, found: &mut impl FnMut(usize)) {
    match node {
        Node::File(at) => found(*at),
        Node::List(items) => items.iter().for_each(|item| collect(item, found)),
        Node::Map(fields) => fields.iter().for_each(|(_, node)| collect(node, found)),
    }
}

/// The rows of `participants.tsv`, each as its columns and values.
fn participants(root: &Path) -> Vec<Vec<(String, String)>> {
    let text = std::fs::read_to_string(root.join("participants.tsv")).unwrap_or_default();
    let mut lines = text.lines();
    let header: Vec<String> = lines
        .next()
        .unwrap_or_default()
        .split('\t')
        .map(|c| c.trim().to_owned())
        .collect();
    lines
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            header
                .iter()
                .cloned()
                .zip(line.split('\t').map(|c| c.trim().to_owned()))
                .collect()
        })
        .collect()
}

/// The `CompositeKey` of the operators: the values of a key closure that gave a list.
#[derive(Clone, Debug)]
struct CompositeKey(Vec<Value>);

impl Native for CompositeKey {
    fn plugin(&self) -> &'static str {
        "nf-bids"
    }

    fn text(&self) -> String {
        format!("CompositeKey({})", Value::list(self.0.clone()))
    }

    fn key(&self) -> Option<String> {
        Some(encoded(&self.0))
    }
}

/// The components of a key with their classes and text lengths: printed alone, `['A, B', 'C']`
/// and `['A', 'B, C']` would give the same key.
fn encoded(values: &[Value]) -> String {
    let part = |value: &Value| match value {
        Value::List(list) => format!("[{}]", encoded(&list.borrow().items)),
        other => {
            let text = other.to_string();
            format!("{}:{}:{text}", other.java_class_name(), text.len())
        }
    };
    values.iter().map(part).collect()
}

impl CompositeKey {
    fn value(self) -> Value {
        Value::Object(Rc::new(RefCell::new(Object {
            class: "nfneuro.plugin.util.CompositeKey",
            state: ObjectState::Plugin(Box::new(self)),
        })))
    }
}

fn items(value: &Value) -> Vec<Value> {
    match value {
        Value::List(list) => list.borrow().items.clone(),
        _ => Vec::new(),
    }
}

/// The pair of a key and an item, as `[key, item]`.
fn pair(value: &Value) -> (Value, Value) {
    let parts = items(value);
    (
        parts.first().cloned().unwrap_or(Value::Null),
        parts.get(1).cloned().unwrap_or(Value::Null),
    )
}

/// `groupTupleBy`: `[key, items]` per key in the order keys come; with a size, a group goes
/// as soon as it is full, and the rest at the end unless `remainder` is false.
fn grouped(pairs: &[Value], size: usize, remainder: bool) -> Value {
    let mut open: Vec<(Value, Vec<Value>)> = Vec::new();
    let mut done = Vec::new();
    for value in pairs {
        let (key, item) = pair(value);
        let at = match open
            .iter()
            .position(|(known, _)| Key(known.clone()) == Key(key.clone()))
        {
            Some(at) => at,
            None => {
                open.push((key, Vec::new()));
                open.len() - 1
            }
        };
        open[at].1.push(item);
        if size > 0 && open[at].1.len() == size {
            let (key, items) = open.remove(at);
            done.push(Value::list(vec![key, Value::list(items)]));
        }
    }
    if size == 0 || remainder {
        done.extend(
            open.into_iter()
                .map(|(key, items)| Value::list(vec![key, Value::list(items)])),
        );
    }
    Value::list(done)
}

/// `joinBy` and `combineBy`: every left item with every right item of an equal key, fused;
/// with `remainder`, then the unmatched items of either side.
fn paired(left: &[Value], right: &[Value], remainder: bool) -> Value {
    let left: Vec<(Value, Value)> = left.iter().map(pair).collect();
    let right: Vec<(Value, Value)> = right.iter().map(pair).collect();
    let equal = |a: &Value, b: &Value| Key(a.clone()) == Key(b.clone());
    let mut fused = Vec::new();
    for (key, item) in &left {
        for (other, value) in &right {
            if equal(key, other) {
                fused.push(fuse(item, value));
            }
        }
    }
    if remainder {
        let unmatched = |side: &[(Value, Value)], other: &[(Value, Value)]| -> Vec<Value> {
            (side.iter())
                .filter(|(key, _)| !other.iter().any(|(known, _)| equal(key, known)))
                .map(|(_, item)| item.clone())
                .collect()
        };
        fused.extend(unmatched(&left, &right));
        fused.extend(unmatched(&right, &left));
    }
    Value::list(fused)
}

/// Maps merge with the right values winning; lists join; other items make a pair.
fn fuse(left: &Value, right: &Value) -> Value {
    match (left, right) {
        (Value::Map(_), Value::Map(_)) => {
            let mut merged: Vec<(String, Value)> = entries(left);
            for (key, value) in entries(right) {
                match merged.iter_mut().find(|(known, _)| *known == key) {
                    Some((_, slot)) => *slot = value,
                    None => merged.push((key, value)),
                }
            }
            Value::map(
                merged
                    .into_iter()
                    .map(|(key, value)| (Value::string(key), value)),
            )
        }
        (Value::List(_), Value::List(_)) => {
            Value::list(items(left).into_iter().chain(items(right)).collect())
        }
        _ => Value::list(vec![left.clone(), right.clone()]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_compare_components() {
        let key = |parts: &[&str]| {
            let parts = parts.iter().map(|part| Value::string(*part)).collect();
            CompositeKey(parts).value()
        };
        let pairs = [
            Value::list(vec![key(&["A, B", "C"]), Value::integer(1)]),
            Value::list(vec![key(&["A", "B, C"]), Value::integer(2)]),
            Value::list(vec![key(&["A", "B, C"]), Value::integer(3)]),
        ];
        let groups = items(&grouped(&pairs, 0, true));
        let sizes: Vec<usize> = groups.iter().map(|group| items(&pair(group).1).len()).collect();
        assert_eq!(sizes, [1, 2]);
    }
}
