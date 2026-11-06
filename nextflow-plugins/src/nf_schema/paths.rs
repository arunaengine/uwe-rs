//! Checks nf-schema path formats and existence through the host before validation, and the
//! keywords of its own: `deprecated` and `uniqueEntries`.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

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

#[derive(Default)]
struct Paths {
    known: HashMap<String, Named>,
    pending: HashSet<String>,
}

type Lookup = Arc<Mutex<Paths>>;

/// What a check adds to plain JSON Schema validation.
#[derive(Clone, Default)]
pub(crate) struct Check {
    /// `exists` only checks values with a path format, as nf-validation 1.1.3 does.
    pub path_exists: bool,
    /// nf-schema's own keywords: `deprecated` and `uniqueEntries`.
    pub keywords: bool,
    /// From nf-schema 2.8.0: its wording for directories, and no directory checks on Azure.
    pub newer: bool,
    /// The folder of the schema.
    pub base: Option<String>,
}

/// A validator with nf-schema's path keywords. Other formats are not checked, as before.
/// With `path_exists`, `exists` only checks values whose format is a path, as nf-validation
/// 1.1.3 does.
pub(crate) fn validator(
    schema: &Json,
    instance: &Json,
    context: &mut Context,
    path_exists: bool,
) -> Result<jsonschema::Validator, String> {
    let check = Check {
        path_exists,
        ..Check::default()
    };
    checker(schema, instance, context, &check)
}

/// A validator with the path keywords and, as `check` asks, nf-schema's own keywords.
pub(crate) fn checker(
    schema: &Json,
    instance: &Json,
    context: &mut Context,
    check: &Check,
) -> Result<jsonschema::Validator, String> {
    let launch = context.host.launch_dir();
    let lookup: Lookup = Arc::default();
    let formats = lookup.clone();
    let exists = lookup.clone();
    let (path_exists, newer) = (check.path_exists, check.newer);
    let mut options = jsonschema::options()
        .with_keyword("format", move |_, value, _| {
            Ok(Box::new(PathFormat {
                format: value.as_str().unwrap_or_default().to_owned(),
                lookup: formats.clone(),
                newer,
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
                lookup: exists.clone(),
            }))
        });
    if check.keywords {
        options = options
            .with_keyword("deprecated", |_, value, _| {
                Ok(Box::new(Deprecated(value.as_bool() == Some(true))))
            })
            .with_keyword("uniqueEntries", |_, value, _| {
                let fields = (value.as_array().into_iter().flatten())
                    .filter_map(|field| field.as_str().map(str::to_owned))
                    .collect();
                Ok(Box::new(Unique(fields)))
            });
    }
    let validator = options.build(schema).map_err(|error| error.to_string())?;
    // Deferred keyword lookups let the validator itself resolve references and branches.
    loop {
        for _ in validator.iter_errors(instance) {}
        let pending = std::mem::take(&mut lookup.lock().expect("path lookup lock").pending);
        if pending.is_empty() {
            break;
        }
        for text in pending {
            let named = look_up(&text, &launch, context);
            lookup
                .lock()
                .expect("path lookup lock")
                .known
                .insert(text, named);
        }
    }
    Ok(validator)
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

struct PathFormat {
    format: String,
    lookup: Lookup,
    /// nf-schema 2.8.0 or later.
    newer: bool,
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
        let mut lookup = self.lookup.lock().expect("path lookup lock");
        let Some(named) = lookup.known.get(text) else {
            lookup.pending.insert(text.to_owned());
            return Some(format!(
                "could not validate file format of '{text}': missing path lookup"
            ));
        };
        // From 2.8.0 on, the type of an Azure path is not checked.
        let azure = self.newer && text.starts_with("az://");
        match (kind, named) {
            ("directory", Named::Failed(message)) if self.newer => Some(format!(
                "could not validate directory format of '{text}': {message}"
            )),
            (_, Named::Failed(message)) => Some(format!(
                "could not validate file format of '{text}': {message}"
            )),
            _ if azure => None,
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
        let mut lookup = self.lookup.lock().expect("path lookup lock");
        let exists = match lookup.known.get(text) {
            Some(Named::Failed(message)) => {
                return Some(format!("could not check existence of '{text}': {message}"));
            }
            Some(Named::Pattern(files)) => !files.is_empty(),
            Some(Named::Path(_, found)) => found.is_some(),
            None => {
                lookup.pending.insert(text.to_owned());
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

/// `deprecated: true`: any value is an error.
struct Deprecated(bool);

impl<'i> Keyword<'i> for Deprecated {
    fn validate(&self, _: &'i Json) -> Result<(), ValidationError<'i>> {
        match self.0 {
            true => Err(ValidationError::custom("This option is deprecated")),
            false => Ok(()),
        }
    }

    fn is_valid(&self, _: &'i Json) -> bool {
        !self.0
    }
}

/// `uniqueEntries`: no two entries of a list may have the same values in these fields.
struct Unique(Vec<String>);

impl Unique {
    fn problems(&self, instance: &Json) -> Vec<String> {
        let Some(entries) = instance.as_array() else {
            return Vec::new();
        };
        let mut seen: Vec<Vec<Json>> = Vec::new();
        let mut problems = Vec::new();
        for (index, entry) in entries.iter().enumerate() {
            let values: Vec<Json> = (self.0.iter())
                .map(|field| entry.get(field).cloned().unwrap_or(Json::Null))
                .collect();
            if seen.contains(&values) {
                let fields: Vec<String> = (self.0.iter().zip(&values))
                    .map(|(field, value)| format!("{field}:{}", super::plain(value)))
                    .collect();
                problems.push(format!(
                    "Entry {}: Detected duplicate entries: [{}]",
                    index + 1,
                    fields.join(", ")
                ));
            } else {
                seen.push(values);
            }
        }
        problems
    }
}

impl<'i> Keyword<'i> for Unique {
    fn validate(&self, instance: &'i Json) -> Result<(), ValidationError<'i>> {
        match self.problems(instance).into_iter().next() {
            Some(problem) => Err(ValidationError::custom(problem)),
            None => Ok(()),
        }
    }

    fn is_valid(&self, instance: &'i Json) -> bool {
        self.problems(instance).is_empty()
    }

    fn iter_errors(
        &self,
        instance: &'i Json,
    ) -> Box<dyn Iterator<Item = ValidationError<'i>> + 'i> {
        let errors: Vec<ValidationError<'i>> = (self.problems(instance).into_iter())
            .map(|problem| ValidationError::custom(problem))
            .collect();
        Box::new(errors.into_iter())
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
    fn skips_plain_strings() {
        #[derive(Default)]
        struct Counting(Vec<String>);
        impl Host for Counting {
            fn print(&mut self, _: &str) {}
            fn metadata(
                &mut self,
                path: &PathValue,
            ) -> Result<
                Option<nextflow_evaluator::interpreter::FileMetadata>,
                nextflow_evaluator::interpreter::Error,
            > {
                self.0.push(path.uri());
                Ok(None)
            }
            fn list(
                &mut self,
                _: &PathValue,
            ) -> Result<Vec<(String, bool)>, nextflow_evaluator::interpreter::Error> {
                panic!("plain strings must not expand globs");
            }
        }
        let mut host = Counting::default();
        let config = Config::default();
        let mut context = Context {
            host: &mut host,
            config: &config,
            params: Value::map([]),
            project_dir: "/",
            cli_params: &[],
            workflow: Value::Null,
        };
        let schema = json!({
            "$defs": {"file": {"$anchor": "file", "format": "file-path", "exists": true}},
            "properties": {"label": {"type": "string"}, "url": {"format": "uri"},
                "files": {"type": "array", "items": {"$ref": "#file"}}}
        });
        let instance = json!({"label": "**", "url": "https://example.invalid/metadata", "files": ["needed.txt", "needed.txt"]});
        assert!(
            !validator(&schema, &instance, &mut context, false)
                .unwrap()
                .is_valid(&instance)
        );
        assert_eq!(host.0.len(), 1);
        assert!(host.0[0].ends_with("/needed.txt"));
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
            workflow: Value::Null,
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
