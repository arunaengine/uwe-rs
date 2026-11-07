//! File and Directory values, and the run-time check of a JSON value against a type.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::types::{FileType, KeyType, Type, TypeDef, resolve};

/// A File or Directory value in its JSON form, tagged by `class` like CWL.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "class")]
pub enum Entry {
    File(FileValue),
    Directory(DirectoryValue),
}

/// A file. It needs a `location` or, for a file made from text, `contents`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileValue {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// The file name; without one it is the last segment of `location`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basename: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// A digest such as `sha1$<hex>` or `sha256:<hex>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checksum: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secondary_files: Vec<Entry>,
    /// The text of the file, when it was loaded or the file is made from it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contents: Option<String>,
}

/// A directory. It needs a `location` or a `listing`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectoryValue {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basename: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listing: Option<Vec<Entry>>,
}

/// Whether a JSON value is a value of the type: the check behind `dynamic` ports. An `int`
/// is a JSON integer in the signed 64-bit range; a `float` is any JSON number.
pub fn fits(value: &Json, r#type: &Type, defs: &[TypeDef]) -> bool {
    let Some(r#type) = resolve(r#type, defs) else {
        return false;
    };
    match r#type {
        Type::Dynamic => true,
        Type::Optional(inner) => value.is_null() || fits(value, inner, defs),
        Type::Bool => value.is_boolean(),
        Type::Int => value.is_i64(),
        Type::Float => value.is_number(),
        Type::String => value.is_string(),
        Type::Enum(symbols) => value
            .as_str()
            .is_some_and(|text| symbols.iter().any(|s| s == text)),
        Type::File(want) => match Entry::deserialize(value) {
            Ok(Entry::File(file)) => file_fits(&file, want),
            _ => false,
        },
        Type::Directory => match Entry::deserialize(value) {
            Ok(Entry::Directory(folder)) => folder.location.is_some() || folder.listing.is_some(),
            _ => false,
        },
        Type::Record(fields) => value.as_object().is_some_and(|object| {
            object
                .keys()
                .all(|key| fields.iter().any(|field| field.name == *key))
                && fields.iter().all(|field| match object.get(&field.name) {
                    Some(item) => fits(item, &field.r#type, defs),
                    None => field.r#type.is_optional(defs),
                })
        }),
        Type::Array(array) => value.as_array().is_some_and(|items| {
            (!array.nonempty || !items.is_empty())
                && items.iter().all(|item| fits(item, &array.items, defs))
        }),
        Type::Map(map) => value.as_object().is_some_and(|object| {
            (object.iter())
                .all(|(key, item)| key_fits(key, map.key) && fits(item, &map.value, defs))
        }),
        Type::Union(members) => members.iter().any(|item| fits(value, item, defs)),
        Type::Tuple(items) => value.as_array().is_some_and(|values| {
            values.len() == items.len()
                && values
                    .iter()
                    .zip(items)
                    .all(|(item, want)| fits(item, want, defs))
        }),
        Type::Named(_) => false,
    }
}

fn file_fits(file: &FileValue, want: &FileType) -> bool {
    if file.location.is_none() && file.contents.is_none() {
        return false;
    }
    if want.format.is_some() && want.format != file.format {
        return false;
    }
    let Some(primary) = file_name(file) else {
        return want.secondary_files.iter().all(|item| !item.required);
    };
    let names: Vec<&str> = (file.secondary_files.iter())
        .filter_map(|entry| match entry {
            Entry::File(item) => file_name(item),
            Entry::Directory(item) => item.basename.as_deref().or(last_segment(&item.location)),
        })
        .collect();
    (want.secondary_files.iter())
        .filter(|item| item.required)
        .all(|item| names.contains(&secondary_name(primary, &item.pattern).as_str()))
}

/// The secondary file name of a pattern: each leading `^` removes one extension of the
/// primary name, then the rest is appended, as in CWL.
pub fn secondary_name(primary: &str, pattern: &str) -> String {
    let mut base = primary;
    let mut rest = pattern;
    while let Some(tail) = rest.strip_prefix('^') {
        base = base.rsplit_once('.').map_or(base, |(head, _)| head);
        rest = tail;
    }
    format!("{base}{rest}")
}

fn file_name(file: &FileValue) -> Option<&str> {
    file.basename.as_deref().or(last_segment(&file.location))
}

fn last_segment(location: &Option<String>) -> Option<&str> {
    let location = location.as_deref()?.trim_end_matches('/');
    location.rsplit('/').next().filter(|name| !name.is_empty())
}

fn key_fits(key: &str, r#type: KeyType) -> bool {
    match r#type {
        KeyType::String => true,
        KeyType::Int => key
            .parse::<i64>()
            .is_ok_and(|number| number.to_string() == key),
        KeyType::Bool => key == "true" || key == "false",
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn kind(value: Json) -> Type {
        serde_json::from_value(value).expect("a valid type")
    }

    #[test]
    fn numbers_follow_rules() {
        assert!(fits(&json!(3), &Type::Int, &[]));
        assert!(fits(&json!(3), &Type::Float, &[]));
        assert!(!fits(&json!(3.5), &Type::Int, &[]));
        assert!(!fits(&json!(3.0), &Type::Int, &[]));
        assert!(!fits(&json!(u64::MAX), &Type::Int, &[]));
        assert!(!fits(&json!("3"), &Type::Int, &[]));
    }

    #[test]
    fn files_keep_secondaries() {
        let indexed = kind(json!({"file": {
            "format": "http://edamontology.org/format_1929",
            "secondary_files": [{"pattern": ".fai"}, {"pattern": "^.dict", "required": false}]
        }}));
        let mut value = json!({
            "class": "File",
            "location": "data/ref.fa",
            "format": "http://edamontology.org/format_1929",
            "secondary_files": [{"class": "File", "location": "data/ref.fa.fai"}]
        });
        assert!(fits(&value, &indexed, &[]));
        value["secondary_files"] = json!([{"class": "File", "location": "data/ref.dict"}]);
        assert!(!fits(&value, &indexed, &[]));
        value["format"] = json!("http://edamontology.org/format_2572");
        assert!(!fits(
            &value,
            &kind(json!({"file": {"format": "http://edamontology.org/format_1929"}})),
            &[]
        ));
        assert!(!fits(
            &json!({"class": "Directory", "location": "x"}),
            &kind(json!({"file": {}})),
            &[]
        ));
        let extra = json!({"class": "File", "location": "a", "extra": 1});
        assert!(!fits(&extra, &kind(json!({"file": {}})), &[]));
        assert_eq!(secondary_name("reads.bam", "^.bai"), "reads.bai");
    }

    #[test]
    fn directories_need_content() {
        let listing = json!({"class": "Directory", "basename": "out", "listing": [
            {"class": "File", "basename": "a.txt", "contents": "a"}
        ]});
        assert!(fits(&listing, &Type::Directory, &[]));
        assert!(!fits(&json!({"class": "Directory"}), &Type::Directory, &[]));
    }

    #[test]
    fn structures_match_exactly() {
        let record = kind(json!({"record": [
            {"name": "id", "type": "string"}, {"name": "size", "type": {"optional": "int"}}
        ]}));
        assert!(fits(&json!({"id": "a"}), &record, &[]));
        assert!(!fits(&json!({"id": "a", "other": 1}), &record, &[]));
        let map = kind(json!({"map": {"key": "int", "value": "bool"}}));
        assert!(fits(&json!({"1": true, "-2": false}), &map, &[]));
        assert!(!fits(&json!({"01": true}), &map, &[]));
        let pair = kind(json!({"tuple": ["string", "int"]}));
        assert!(fits(&json!(["a", 1]), &pair, &[]));
        assert!(!fits(&json!(["a"]), &pair, &[]));
        let nonempty = kind(json!({"array": {"items": "int", "nonempty": true}}));
        assert!(!fits(&json!([]), &nonempty, &[]));
        assert!(fits(&json!(null), &kind(json!({"optional": "int"})), &[]));
        assert!(fits(&json!({"any": [1]}), &Type::Dynamic, &[]));
    }
}
