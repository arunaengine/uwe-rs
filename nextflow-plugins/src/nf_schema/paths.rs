//! nf-schema's own keywords for paths: the formats `file-path`, `directory-path`, `path` and
//! `file-path-pattern`, and `exists`. They look at the files that texts name, which are
//! looked up through the host before validation.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use jsonschema::{Keyword, ValidationError};
use nextflow_evaluator::interpreter::glob::{GlobOptions, glob, is_glob};
use nextflow_evaluator::value::PathValue;
use serde_json::Value as Json;

use crate::Context;

const PATH_FORMATS: &[&str] = &["file-path", "directory-path", "path", "file-path-pattern"];

/// What a text names on the file system.
enum Named {
    /// A glob pattern: the matching files, each with whether it is a directory.
    Pattern(Vec<(String, bool)>),
    /// One path, with `None` when it does not exist, else whether it is a directory.
    Path(String, Option<bool>),
    /// The file system could not tell.
    Failed(String),
}

type Lookup = Arc<HashMap<String, Named>>;

/// A validator with nf-schema's path keywords. Other formats are not checked, as before.
/// With `path_exists`, `exists` only checks values whose format is a path, as nf-validation
/// 1.1.3 does.
pub(crate) fn validator(
    schema: &Json,
    instance: &Json,
    context: &mut Context,
    path_exists: bool,
) -> Result<jsonschema::Validator, String> {
    let mut names = HashSet::new();
    path_properties(schema, &mut names);
    // A list of plain values, like the rows of a samplesheet without header.
    let all = schema
        .get("items")
        .is_some_and(|items| items.get("properties").is_none() && checks_paths(items));
    let mut texts = HashSet::new();
    path_texts(instance, &names, all, &mut texts);
    let launch = context.host.launch_dir();
    let lookup: Lookup = Arc::new(
        texts
            .into_iter()
            .map(|text| {
                let named = look_up(&text, &launch, context);
                (text, named)
            })
            .collect(),
    );
    let formats = lookup.clone();
    jsonschema::options()
        .with_keyword("format", move |_, value, _| {
            Ok(Box::new(PathFormat {
                format: value.as_str().unwrap_or_default().to_owned(),
                lookup: formats.clone(),
            }))
        })
        .with_keyword("exists", move |parent, value, _| {
            let format = parent
                .get("format")
                .and_then(Json::as_str)
                .unwrap_or_default();
            let checked = !path_exists || ["file-path", "directory-path", "path"].contains(&format);
            Ok(Box::new(Exists {
                wanted: value.as_bool().filter(|_| checked),
                lookup: lookup.clone(),
            }))
        })
        .build(schema)
        .map_err(|error| error.to_string())
}

fn look_up(text: &str, launch: &str, context: &mut Context) -> Named {
    if is_glob(text) {
        let paths = match glob(context.host, text, &GlobOptions::default()) {
            Ok(paths) => paths,
            Err(error) => return Named::Failed(error.to_string()),
        };
        let files = paths
            .iter()
            .map(|path| {
                let directory = context
                    .host
                    .metadata(path)
                    .ok()
                    .flatten()
                    .is_some_and(|metadata| metadata.directory);
                (path.text().to_owned(), directory)
            })
            .collect();
        return Named::Pattern(files);
    }
    let path = PathValue::parse(text, launch);
    match context.host.metadata(&path) {
        Ok(found) => Named::Path(
            path.text().to_owned(),
            found.map(|metadata| metadata.directory),
        ),
        Err(error) => Named::Failed(error.to_string()),
    }
}

/// The names of the properties whose schema checks paths, at any depth.
fn path_properties(schema: &Json, names: &mut HashSet<String>) {
    match schema {
        Json::Object(map) => {
            if let Some(Json::Object(properties)) = map.get("properties") {
                for (name, definition) in properties {
                    if checks_paths(definition) {
                        names.insert(name.clone());
                    }
                }
            }
            map.values().for_each(|value| path_properties(value, names));
        }
        Json::Array(items) => items.iter().for_each(|item| path_properties(item, names)),
        _ => {}
    }
}

fn checks_paths(schema: &Json) -> bool {
    match schema {
        Json::Object(map) => {
            let format = map.get("format").and_then(Json::as_str);
            format.is_some_and(|format| PATH_FORMATS.contains(&format))
                || map.get("exists").is_some_and(Json::is_boolean)
                || map.values().any(checks_paths)
        }
        Json::Array(items) => items.iter().any(checks_paths),
        _ => false,
    }
}

/// The texts under the properties in `names`, or all texts when `all`.
fn path_texts(instance: &Json, names: &HashSet<String>, all: bool, texts: &mut HashSet<String>) {
    match instance {
        Json::String(text) if all => {
            texts.insert(text.clone());
        }
        Json::Array(items) => items
            .iter()
            .for_each(|item| path_texts(item, names, all, texts)),
        Json::Object(map) => {
            for (key, value) in map {
                path_texts(value, names, all || names.contains(key), texts);
            }
        }
        _ => {}
    }
}

struct PathFormat {
    format: String,
    lookup: Lookup,
}

impl PathFormat {
    /// The problem nf-schema reports for a text, if any.
    fn problem(&self, instance: &Json) -> Option<String> {
        let text = instance.as_str()?;
        let named = self.lookup.get(text)?;
        let kind = match self.format.as_str() {
            "file-path" => "file",
            "directory-path" => "directory",
            "path" => "path",
            "file-path-pattern" => "",
            _ => return None,
        };
        match (kind, named) {
            (_, Named::Failed(message)) => Some(format!(
                "could not validate file format of '{text}': {message}"
            )),
            ("", Named::Pattern(files)) if files.is_empty() => Some(format!(
                "No files were found using the glob pattern '{text}'"
            )),
            ("", Named::Pattern(files)) => {
                let directories: Vec<String> = files
                    .iter()
                    .filter(|(_, directory)| *directory)
                    .map(|(file, _)| format!("'{file}' is not a file, but a directory"))
                    .collect();
                (!directories.is_empty()).then(|| directories.join("\n"))
            }
            ("", Named::Path(file, Some(true))) => {
                Some(format!("'{file}' is not a file, but a directory"))
            }
            (_, Named::Pattern(_)) if !kind.is_empty() => {
                Some(format!("'{text}' is not a {kind}, but a file path pattern"))
            }
            ("file", Named::Path(_, Some(true))) => {
                Some(format!("'{text}' is not a file, but a directory"))
            }
            ("directory", Named::Path(_, Some(false))) => {
                Some(format!("'{text}' is not a directory, but a file"))
            }
            _ => None,
        }
    }
}

impl<'i> Keyword<'i> for PathFormat {
    fn validate(&self, instance: &'i Json) -> Result<(), ValidationError<'i>> {
        match self.problem(instance) {
            Some(message) => Err(ValidationError::custom(message)),
            None => Ok(()),
        }
    }

    fn is_valid(&self, instance: &'i Json) -> bool {
        self.problem(instance).is_none()
    }
}

struct Exists {
    /// Whether the file should exist; `None` for a value that is not a boolean.
    wanted: Option<bool>,
    lookup: Lookup,
}

impl Exists {
    fn problem(&self, instance: &Json) -> Option<String> {
        let wanted = self.wanted?;
        let text = instance.as_str()?;
        let exists = match self.lookup.get(text)? {
            Named::Failed(message) => {
                return Some(format!("could not check existence of '{text}': {message}"));
            }
            Named::Pattern(files) => !files.is_empty(),
            Named::Path(_, found) => found.is_some(),
        };
        match (exists, wanted) {
            (false, true) => Some(format!("the file or directory '{text}' does not exist")),
            (true, false) => Some(format!("the file or directory '{text}' should not exist")),
            _ => None,
        }
    }
}

impl<'i> Keyword<'i> for Exists {
    fn validate(&self, instance: &'i Json) -> Result<(), ValidationError<'i>> {
        match self.problem(instance) {
            Some(message) => Err(ValidationError::custom(message)),
            None => Ok(()),
        }
    }

    fn is_valid(&self, instance: &'i Json) -> bool {
        self.problem(instance).is_none()
    }
}
