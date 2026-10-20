//! Checks nf-schema path formats and existence through the host before validation.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use jsonschema::{Keyword, ValidationError};
use nextflow_evaluator::interpreter::glob::{GlobOptions, glob, is_glob};
use nextflow_evaluator::value::PathValue;
use serde_json::Value as Json;

use crate::Context;

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
    let mut texts = HashSet::new();
    path_texts(instance, &mut texts);
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

/// All string leaves, so schema references and branches remain the validator's concern.
fn path_texts(instance: &Json, texts: &mut HashSet<String>) {
    match instance {
        Json::String(text) => {
            texts.insert(text.clone());
        }
        Json::Array(items) => items.iter().for_each(|item| path_texts(item, texts)),
        Json::Object(map) => map.values().for_each(|value| path_texts(value, texts)),
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
        let kind = match self.format.as_str() {
            "file-path" => "file",
            "directory-path" => "directory",
            "path" => "path",
            "file-path-pattern" => "",
            _ => return None,
        };
        let Some(named) = self.lookup.get(text) else {
            return Some(format!(
                "could not validate file format of '{text}': missing path lookup"
            ));
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
        let exists = match self.lookup.get(text) {
            Some(Named::Failed(message)) => {
                return Some(format!("could not check existence of '{text}': {message}"));
            }
            Some(Named::Pattern(files)) => !files.is_empty(),
            Some(Named::Path(_, found)) => found.is_some(),
            None => {
                return Some(format!(
                    "could not check existence of '{text}': missing path lookup"
                ));
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use nextflow_config::Config;
    use nextflow_evaluator::interpreter::Host;
    use nextflow_evaluator::value::Value;
    use serde_json::json;

    struct Local(String);

    impl Host for Local {
        fn print(&mut self, _: &str) {}

        fn launch_dir(&self) -> String {
            self.0.clone()
        }
    }

    #[test]
    fn checks_nested_paths() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("file.txt"), b"data").unwrap();
        std::fs::create_dir(directory.path().join("folder")).unwrap();
        let launch = directory.path().to_string_lossy().into_owned();
        let mut host = Local(launch.clone());
        let config = Config::default();
        let mut context = Context {
            host: &mut host,
            config: &config,
            params: Value::map([]),
            project_dir: &launch,
            cli_params: &[],
        };
        let path = json!({"type": "string", "format": "file-path", "exists": true});
        let reference = json!({"$defs": {"file": path}, "$ref": "#/$defs/file"});
        for value in ["missing.txt", "folder", "file.txt"] {
            let valid = value == "file.txt";
            for (schema, instance) in [
                (path.clone(), json!(value)),
                (reference.clone(), json!(value)),
                (
                    json!({"$defs": {"file": path}, "properties": {"input": {"$ref": "#/$defs/file"}}}),
                    json!({"input": value}),
                ),
                (
                    json!({"type": "array", "$defs": {"file": path}, "items": {"$ref": "#/$defs/file"}}),
                    json!([value]),
                ),
                (
                    json!({"properties": {"input": path}}),
                    json!({"input": value}),
                ),
            ] {
                assert_eq!(
                    validator(&schema, &instance, &mut context, false)
                        .unwrap()
                        .is_valid(&instance),
                    valid,
                    "{schema}: {instance}"
                );
            }
        }
        let schema = json!({"anyOf": [path, {"const": "missing.txt"}]});
        let instance = json!("missing.txt");
        assert!(
            validator(&schema, &instance, &mut context, false)
                .unwrap()
                .is_valid(&instance)
        );
    }
}
