//! The objects that nf-anndata gives scripts, and their methods and printed forms.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::rc::Rc;

use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::value::{
    Key, List, ListCell, ListKind, Native, Object, Set, SetKind, State as ObjectState, Value,
};

use super::{Data, Frame, NAMED, column, java_order};
use crate::Context;

/// The objects that nf-anndata gives scripts.
#[derive(Clone, Debug)]
pub(super) enum Anndata {
    Data(Rc<Data>),
    Frame(Rc<Frame>),
    Index(Rc<Frame>),
    Column(Rc<[Value]>),
}

impl Native for Anndata {
    fn plugin(&self) -> &'static str {
        "nf-anndata"
    }

    fn text(&self) -> String {
        match self {
            Anndata::Data(data) => data.text(),
            // Java's `Object.toString()`; the plugin's classes do not override it.
            other => format!("{}@{:x}", other.class(), other.address()),
        }
    }
}

impl Anndata {
    fn class(&self) -> &'static str {
        match self {
            Anndata::Data(_) => "nictru.nf.anndata.AnnData",
            Anndata::Frame(_) => "nictru.nf.anndata.DataFrame",
            Anndata::Index(_) => "nictru.nf.anndata.DataFrameIndex",
            Anndata::Column(_) => "nictru.nf.anndata.DataFrameColumn",
        }
    }

    /// The address of the shared state, which stands in for Java's identity hash.
    fn address(&self) -> usize {
        match self {
            Anndata::Data(data) => Rc::as_ptr(data) as usize,
            Anndata::Frame(frame) | Anndata::Index(frame) => Rc::as_ptr(frame) as usize,
            Anndata::Column(values) => Rc::as_ptr(values).cast::<Value>() as usize,
        }
    }

    pub(super) fn value(self) -> Value {
        Value::Object(Rc::new(RefCell::new(Object {
            class: self.class(),
            state: ObjectState::Plugin(Box::new(self)),
        })))
    }
}

impl Data {
    /// The `yaml` map: sizes, frames by index and columns, and the sorted names of the groups.
    fn yaml(&self) -> Value {
        let text = |text: &str| Value::string(text);
        let frame = |frame: &Frame| {
            Value::map([
                (text("index"), text(&frame.index)),
                (
                    text("columns"),
                    Value::list(frame.columns.iter().map(|c| text(c)).collect()),
                ),
            ])
        };
        let mut entries = vec![
            (text("n_obs"), size(self.obs.rows.len())),
            (text("n_vars"), size(self.var.rows.len())),
            (text("obs"), frame(&self.obs)),
            (text("var"), frame(&self.var)),
        ];
        for (group, names) in NAMED.iter().zip(&self.names) {
            let mut sorted = names.clone();
            sorted.sort_by(|a, b| java_order(a, b));
            entries.push((
                text(group),
                Value::list(sorted.iter().map(|n| text(n)).collect()),
            ));
        }
        Value::map(entries)
    }

    /// The summary of `toString()`, with a line for each group or frame that has names.
    fn text(&self) -> String {
        let (observations, variables) = (self.obs.rows.len(), self.var.rows.len());
        let mut lines = vec![format!(
            "AnnData object with n_obs × n_vars = {observations} × {variables}"
        )];
        let groups = [("obs", &self.obs.columns), ("var", &self.var.columns)];
        let named = NAMED.iter().copied().zip(&self.names);
        for (key, names) in groups.into_iter().chain(named) {
            if !names.is_empty() {
                let quoted: Vec<String> = names.iter().map(|name| format!("'{name}'")).collect();
                lines.push(format!("\t{key}: {}", quoted.join(", ")));
            }
        }
        lines.join("\n")
    }
}

fn size(count: usize) -> Value {
    Value::integer(i32::try_from(count).unwrap_or(i32::MAX))
}

/// A `String[]`.
fn strings(texts: &[String]) -> Value {
    let items = texts
        .iter()
        .map(|text| Value::string(text.as_str()))
        .collect();
    Value::List(Rc::new(ListCell::new(List::new(ListKind::Array, items))))
}

/// A `java.util.HashSet` of the values, in Java's iteration order: by bucket of the final
/// table, and in arrival order within a bucket.
fn hash_set(values: impl IntoIterator<Item = Value>) -> Value {
    let mut set = Set {
        kind: SetKind::HashSet(16),
        items: Default::default(),
    };
    for value in values {
        set.items.insert(Key(value));
    }
    let mut capacity = 16;
    while set.items.len() as f64 > 0.75 * capacity as f64 {
        capacity *= 2;
    }
    let bucket = |key: &Key| {
        let hash = key.0.java_hash() as u32;
        ((hash ^ (hash >> 16)) as usize) & (capacity - 1)
    };
    set.items.sort_by(|a, b| bucket(a).cmp(&bucket(b)));
    set.kind = SetKind::HashSet(capacity);
    Value::Set(Rc::new(RefCell::new(set)))
}

/// A property read through its getter, like `getN_obs` for `n_obs`.
fn property(name: &str, arguments: &[Value]) -> String {
    match name.strip_prefix("get") {
        Some(rest) if arguments.is_empty() && !rest.is_empty() => {
            let mut characters = rest.chars();
            let first = characters
                .next()
                .map(|c| c.to_lowercase().collect::<String>());
            format!("{}{}", first.unwrap_or_default(), characters.as_str())
        }
        _ => name.into(),
    }
}

/// Runs a method of an nf-anndata object, `None` for a method it does not have.
pub(super) fn methods(
    object: &Value,
    name: &str,
    arguments: Vec<Value>,
    _: &mut Context,
) -> Result<Option<Value>, Error> {
    let state = match object {
        Value::Object(object) => match &object.borrow().state {
            ObjectState::Plugin(native) => {
                let native: &dyn std::any::Any = &**native;
                native.downcast_ref::<Anndata>().cloned()
            }
            _ => None,
        },
        _ => None,
    };
    let Some(state) = state else {
        return Ok(None);
    };
    let property = property(name, &arguments);
    Ok(Some(
        match (&state, property.as_str(), arguments.as_slice()) {
            (_, "toString", []) => Value::string(state.text()),
            (Anndata::Data(data), "n_obs", []) => size(data.obs.rows.len()),
            (Anndata::Data(data), "n_vars", []) => size(data.var.rows.len()),
            (Anndata::Data(data), "obs", []) => Anndata::Frame(data.obs.clone()).value(),
            (Anndata::Data(data), "var", []) => Anndata::Frame(data.var.clone()).value(),
            (Anndata::Data(data), "obs_names", []) => strings(&data.obs.rows),
            (Anndata::Data(data), "var_names", []) => strings(&data.var.rows),
            (Anndata::Data(data), "yaml", []) => data.yaml(),
            (Anndata::Data(_), "close", []) => Value::Null,
            (Anndata::Data(data), group, []) if NAMED.contains(&group) => {
                let at = NAMED
                    .iter()
                    .position(|name| *name == group)
                    .unwrap_or_default();
                Value::set(
                    data.names[at]
                        .iter()
                        .map(|name| Value::string(name.as_str())),
                )
            }
            (Anndata::Frame(frame), "colnames", []) => strings(&frame.columns),
            (Anndata::Frame(frame), "rownames", []) => strings(&frame.rows),
            // `size` is a property of a frame only; `size()` is not one of its methods.
            (Anndata::Frame(frame), "size", []) if name == "getSize" => size(frame.rows.len()),
            (Anndata::Frame(frame), "index", []) => Anndata::Index(frame.clone()).value(),
            (Anndata::Frame(frame), "get", [column_name]) => {
                let values = column(frame, &column_name.to_string())?;
                Anndata::Column(values.into()).value()
            }
            (Anndata::Index(frame), "name", []) => Value::string(frame.index.as_str()),
            (Anndata::Index(frame), "values", []) => strings(&frame.rows),
            (Anndata::Index(frame), "size", []) => size(frame.rows.len()),
            (Anndata::Index(frame), "unique", []) => {
                hash_set(frame.rows.iter().map(|row| Value::string(row.as_str())))
            }
            (Anndata::Index(frame), "n_unique", []) => {
                let unique = hash_set(frame.rows.iter().map(|row| Value::string(row.as_str())));
                size(set_size(&unique))
            }
            (Anndata::Index(frame), "contains", [value]) => Value::Boolean(match value {
                Value::String(text) | Value::GString(text) => {
                    frame.rows.iter().any(|row| **row == **text)
                }
                _ => false,
            }),
            (Anndata::Column(values), "data", []) => Value::list(values.to_vec()),
            (Anndata::Column(values), "unique", []) => hash_set(values.iter().cloned()),
            (Anndata::Column(values), "n_unique", []) => {
                size(set_size(&hash_set(values.iter().cloned())))
            }
            _ => return Ok(None),
        },
    ))
}

fn set_size(set: &Value) -> usize {
    match set {
        Value::Set(set) => set.borrow().items.len(),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::property;

    #[test]
    fn reads_getter_names() {
        assert_eq!(property("getN_obs", &[]), "n_obs");
        assert_eq!(property("getObs_names", &[]), "obs_names");
        assert_eq!(property("get", &[]), "get");
    }
}
