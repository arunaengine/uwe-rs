//! The value graph of nf-r-ipc: a value as table rows, one node per row with its parent, key or
//! index and tag, and the checked way back from the rows of R's answer.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;

use nextflow_evaluator::number::Number;
use nextflow_evaluator::value::Value;

use super::missing::{self, Kind};
use super::table::{DATA, Row};

/// The rows of the argument map, the root of a request, from id 1 in depth-first order.
pub(crate) fn encode(arguments: &[(String, Value)]) -> Result<Vec<Row>, String> {
    let root = Value::map(
        (arguments.iter()).map(|(key, value)| (Value::string(key.as_str()), value.clone())),
    );
    let mut rows = Vec::new();
    let mut last = 0;
    node(&root, None, None, None, &mut last, &mut rows)?;
    Ok(rows)
}

fn node(
    value: &Value,
    parent: Option<i64>,
    key: Option<String>,
    index: Option<i64>,
    last: &mut i64,
    rows: &mut Vec<Row>,
) -> Result<(), String> {
    let frame = columns(value);
    // The plugin skips one id before each data frame; readers must not expect gaps to be absent.
    if frame {
        *last += 1;
    }
    *last += 1;
    let id = *last;
    let mut row = Row {
        section: Some(DATA.to_owned()),
        id: Some(id),
        parent,
        key,
        index,
        ..Row::default()
    };
    let tag = |row: &mut Row, tag: &str| row.tag = Some(tag.to_owned());
    match value {
        Value::Null => tag(&mut row, "null"),
        Value::String(text) | Value::GString(text) => {
            tag(&mut row, "string");
            row.text = Some(text.to_string());
        }
        Value::Character(_) => {
            tag(&mut row, "string");
            row.text = Some(value.to_string());
        }
        Value::Number(Number::Integer(number)) => {
            tag(&mut row, "int64");
            row.integer = Some(i64::from(*number));
        }
        Value::Number(Number::Long(number)) => {
            tag(&mut row, "int64");
            row.integer = Some(*number);
        }
        Value::Number(Number::Double(_) | Number::Float(_) | Number::BigDecimal(_)) => {
            tag(&mut row, "float64");
            row.float = value.to_string().parse().ok();
        }
        Value::Boolean(flag) => {
            tag(&mut row, "bool");
            row.boolean = Some(*flag);
        }
        Value::List(list) => {
            tag(&mut row, "list");
            rows.push(row);
            let items = list.borrow().items.clone();
            for (place, item) in items.iter().enumerate() {
                let place = i64::try_from(place).unwrap_or(i64::MAX);
                node(item, Some(id), None, Some(place), last, rows)?;
            }
            return Ok(());
        }
        Value::Map(map) => {
            tag(&mut row, if frame { "data_frame" } else { "map" });
            rows.push(row);
            let entries = map.borrow().entries.clone();
            for (name, item) in &entries {
                node(item, Some(id), Some(name.0.to_string()), None, last, rows)?;
            }
            return Ok(());
        }
        other => match missing::kind(other) {
            Some(kind) => tag(&mut row, kind.tag()),
            None => {
                return Err(format!(
                    "Unsupported value type in value graph: {}",
                    other.java_class_name()
                ));
            }
        },
    }
    rows.push(row);
    Ok(())
}

/// Whether a map has the shape of a data frame: lists of scalars of one length.
pub(crate) fn columns(value: &Value) -> bool {
    let Value::Map(map) = value else {
        return false;
    };
    let map = map.borrow();
    let mut lengths = map.entries.values().map(|column| match column {
        Value::List(list) => {
            let list = list.borrow();
            let scalars =
                !(list.items.iter()).any(|item| matches!(item, Value::List(_) | Value::Map(_)));
            scalars.then_some(list.items.len())
        }
        _ => None,
    });
    let Some(Some(first)) = lengths.next() else {
        return false;
    };
    lengths.all(|length| length == Some(first))
}

/// The value of the value rows of an answer, `None` when it has none.
pub(crate) fn decode(rows: &[Row]) -> Result<Option<Value>, String> {
    let data: Vec<&Row> = (rows.iter())
        .filter(|row| row.section.as_deref() == Some(DATA))
        .collect();
    if data.is_empty() {
        return Ok(None);
    }
    let mut places = HashMap::new();
    for (place, row) in data.iter().enumerate() {
        let id = row.id.ok_or("Value graph cannot be empty")?;
        if places.insert(id, place).is_some() {
            return Err(format!("Duplicate value_id in value graph: {id}"));
        }
    }
    let roots: Vec<usize> = (0..data.len())
        .filter(|&place| data[place].parent.is_none())
        .collect();
    if roots.len() != 1 {
        return Err(format!(
            "Value graph must contain exactly one root, found: {}",
            roots.len()
        ));
    }
    let mut children: HashMap<i64, Vec<usize>> = HashMap::new();
    for (place, row) in data.iter().enumerate() {
        if let Some(parent) = row.parent {
            if !places.contains_key(&parent) {
                let id = row.id.unwrap_or_default();
                return Err(format!(
                    "Value graph has missing parent for node {id}: {parent}"
                ));
            }
            children.entry(parent).or_default().push(place);
        }
    }
    let graph = Graph { data, children };
    graph.value(roots[0]).map(Some)
}

struct Graph<'a> {
    data: Vec<&'a Row>,
    children: HashMap<i64, Vec<usize>>,
}

impl Graph<'_> {
    fn value(&self, place: usize) -> Result<Value, String> {
        let row = self.data[place];
        let id = row.id.unwrap_or_default();
        let tag = row.tag.clone().unwrap_or_default();
        let kids = self.children.get(&id).cloned().unwrap_or_default();
        let set = [
            row.text.is_some(),
            row.integer.is_some(),
            row.float.is_some(),
            row.boolean.is_some(),
        ];
        let scalar = |wanted: usize| -> Result<(), String> {
            if !kids.is_empty() {
                return Err(format!(
                    "Scalar/null/NA node cannot have children: parent {id} ({tag})"
                ));
            }
            let fine = (set.iter().enumerate()).all(|(column, on)| *on == (column == wanted));
            match (fine, wanted) {
                (true, _) => Ok(()),
                (false, 4) => Err(format!(
                    "Node {id} ({tag}) must not set scalar value columns"
                )),
                (false, _) => Err(format!("Invalid value columns for {tag} node {id}")),
            }
        };
        match tag.as_str() {
            "null" => scalar(4).map(|()| Value::Null),
            "string" => scalar(0).map(|()| Value::string(row.text.clone().unwrap_or_default())),
            "int64" => scalar(1).map(|()| Value::Number(Number::Long(row.integer.unwrap_or(0)))),
            "float64" => {
                scalar(2).map(|()| Value::Number(Number::Double(row.float.unwrap_or(0.0))))
            }
            "bool" => scalar(3).map(|()| Value::Boolean(row.boolean.unwrap_or(false))),
            "list" => self.list(id, kids),
            "map" | "data_frame" => self.map(id, kids, tag == "data_frame"),
            other => match Kind::from_tag(other) {
                Some(kind) => scalar(4).map(|()| missing::value(kind)),
                None => Err(format!("Unknown value graph tag: {other}")),
            },
        }
    }

    fn list(&self, id: i64, kids: Vec<usize>) -> Result<Value, String> {
        let mut placed = Vec::new();
        for kid in kids {
            let row = self.data[kid];
            let index = row
                .index
                .ok_or_else(|| format!("List child missing index for parent {id}"))?;
            if index < 0 {
                return Err(format!(
                    "List child has negative index {index} for parent {id}"
                ));
            }
            if row.key.is_some() {
                return Err(format!("List child must not set key for parent {id}"));
            }
            if placed.iter().any(|(known, _)| *known == index) {
                return Err(format!(
                    "Duplicate list child index {index} for parent {id}"
                ));
            }
            placed.push((index, kid));
        }
        placed.sort_by_key(|(index, _)| *index);
        let items: Result<Vec<Value>, String> =
            placed.into_iter().map(|(_, kid)| self.value(kid)).collect();
        Ok(Value::list(items?))
    }

    fn map(&self, id: i64, kids: Vec<usize>, frame: bool) -> Result<Value, String> {
        let mut entries: Vec<(String, Value)> = Vec::new();
        for kid in kids {
            let row = self.data[kid];
            let key = (row.key.clone())
                .filter(|key| !key.is_empty())
                .ok_or_else(|| format!("Map/data_frame child missing key for parent {id}"))?;
            if row.index.is_some() {
                return Err(format!(
                    "Map/data_frame child must not set index for parent {id}"
                ));
            }
            if entries.iter().any(|(known, _)| *known == key) {
                return Err(format!(
                    "Duplicate map/data_frame child key '{key}' for parent {id}"
                ));
            }
            let value = self.value(kid)?;
            if frame && !matches!(value, Value::List(_)) {
                return Err(format!(
                    "data_frame column must decode to list for key={key}"
                ));
            }
            entries.push((key, value));
        }
        Ok(Value::map(
            (entries.into_iter()).map(|(key, value)| (Value::string(key), value)),
        ))
    }
}
