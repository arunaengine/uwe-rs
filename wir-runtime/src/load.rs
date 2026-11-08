//! Load ops: the value of an input with the contents of its Files and the listings of its
//! Directories read, through the file access of uwe's CWL driver.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use cwl_parser::iri;
use cwl_runtime::Value;
use cwl_runtime::files::Access;
use serde_json::{Map, Value as Json};
use wir::{Listing, Load, Op};

use crate::cwl::{to_cwl, to_wir};

/// The output values of a load op from its input value.
pub fn load(
    spec: &Load,
    op: &Op,
    bound: &Map<String, Json>,
    access: &Access,
) -> Result<Map<String, Json>, String> {
    let (Some(input), Some(output)) = (op.inputs.first(), op.outputs.first()) else {
        return Err("a load needs one input and one output".into());
    };
    let given = bound.get(&input.name).cloned().unwrap_or(Json::Null);
    let value = read(given, &|value| {
        if let Some(listing) = spec.listing {
            access.load_listings(value, listing == Listing::Deep)?;
        }
        match spec.contents {
            true => contents(value, access),
            false => Ok(()),
        }
    })?;
    Ok(Map::from_iter([(output.name.clone(), value)]))
}

/// A value with the listing of each Directory read.
pub(crate) fn listings(value: Json, listing: Listing, access: &Access) -> Result<Json, String> {
    read(value, &|value| {
        access.load_listings(value, listing == Listing::Deep)
    })
}

/// A value after `change` read files into it as a CWL value.
fn read(value: Json, change: &dyn Fn(&mut Value) -> Result<(), String>) -> Result<Json, String> {
    let mut value = Value::parse(&to_cwl(located(value, &urls)).to_string())?;
    change(&mut value)?;
    let json: Json = serde_json::from_str(&value.to_json()).map_err(|error| error.to_string())?;
    Ok(located(to_wir(json), &paths))
}

/// Reads the text of each File, alone or in arrays.
fn contents(value: &mut Value, access: &Access) -> Result<(), String> {
    match value {
        Value::List(items) => items.iter_mut().try_for_each(|item| contents(item, access)),
        Value::Map(file) if file.get("class").and_then(Value::as_str) == Some("File") => {
            access.load_contents(file)
        }
        _ => Ok(()),
    }
}

/// The value with each `location` changed by `change`.
fn located(value: Json, change: &dyn Fn(&str) -> Option<String>) -> Json {
    match value {
        Json::Array(items) => Json::Array(
            items
                .into_iter()
                .map(|item| located(item, change))
                .collect(),
        ),
        Json::Object(object) => Json::Object(
            (object.into_iter())
                .map(|(key, item)| match (key.as_str(), &item) {
                    ("location", Json::String(text)) => {
                        let changed = change(text).map_or(item, Json::String);
                        (key, changed)
                    }
                    _ => (key, located(item, change)),
                })
                .collect(),
        ),
        other => other,
    }
}

/// A `file:` URL for a path, as the file access of CWL reads it.
fn urls(location: &str) -> Option<String> {
    location
        .starts_with('/')
        .then(|| iri::file_url(Path::new(location)))
}

/// A path for a `file:` URL, as wir values hold it.
fn paths(location: &str) -> Option<String> {
    iri::file_path(location).map(|path| path.to_string_lossy().into_owned())
}
