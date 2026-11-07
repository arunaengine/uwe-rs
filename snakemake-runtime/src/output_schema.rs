//! Names the files of the target rules or files as outputs: their JSON Schema and their values.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use input_schema::{DIRECTORY_PATH, DRAFT, FILE_PATH};
use python_evaluator::{Error, Interpreter, Value};
use serde_json::{Map, Value as Json, json};

use crate::pattern::Wildcards;
use crate::rule::Rule;

/// A file of an output, and whether it is a directory.
type File = (String, bool);

/// The files of one item of a target: its name if it has one, and whether it is a group.
pub type Group = (Option<String>, Vec<File>, bool);

/// An output by its name, with its files and whether it is a group.
type Output = (String, Vec<File>, bool);

/// The schema of the files of the target rules or files.
pub fn schema(
    interpreter: &mut Interpreter,
    rules: &[Rule],
    targets: &[String],
    title: &str,
) -> Result<Json, Error> {
    let mut groups = Vec::new();
    for target in targets {
        let Some(rule) = rules.iter().find(|rule| rule.name == *target) else {
            groups.push((None, vec![(target.clone(), false)], false));
            continue;
        };
        let items = match rule.output.values.is_empty() {
            true => crate::plan::expand_input(interpreter, rule, &Wildcards::new())?.0,
            false => rule.output.clone(),
        };
        for (name, values, many) in items.named_entries() {
            let files = values
                .iter()
                .map(|value| file(interpreter, value))
                .collect::<Result<Vec<File>, Error>>()?;
            groups.push((name, files, many));
        }
    }
    let (named, loose) = named(groups);
    let mut properties: Map<String, Json> = (named.into_iter())
        .map(|(name, files, many)| (name, output(&files, many)))
        .collect();
    if let Some((name, files, many)) = loose {
        let mut schema = output(&files, many);
        schema["description"] = json!("the files that the target rules make or ask for");
        properties.insert(name, schema);
    }
    Ok(json!({"$schema": DRAFT, "title": title, "type": "object", "properties": properties}))
}

/// The result values of the files of the targets, named and shaped as [`schema`] describes
/// them: a File or a Directory at its path from `workdir`, or an array of them for a group.
pub fn results(groups: Vec<Group>, workdir: &Path) -> Json {
    let (named, loose) = named(groups);
    let values = named.into_iter().chain(loose).map(|(name, files, many)| {
        let mut entries: Vec<Json> = files.iter().map(|file| entry(file, workdir)).collect();
        let value = match (entries.len(), many) {
            (1, false) => entries.remove(0),
            _ => Json::Array(entries),
        };
        (name, value)
    });
    Json::Object(values.collect())
}

/// The named items in order, the first of each name, and the other files once each as one
/// group `targets`, with `_` added while a named item has that name.
fn named(groups: Vec<Group>) -> (Vec<Output>, Option<Output>) {
    let mut named: Vec<Output> = Vec::new();
    let mut loose: Vec<File> = Vec::new();
    for (name, files, many) in groups {
        match name {
            Some(name) if !named.iter().any(|(taken, ..)| *taken == name) => {
                named.push((name, files, many));
            }
            _ => loose.extend(files),
        }
    }
    // A file that several target rules ask for is one output.
    let mut seen = std::collections::HashSet::new();
    loose.retain(|(path, _)| seen.insert(path.clone()));
    if loose.is_empty() {
        return (named, None);
    }
    let mut name = "targets".to_owned();
    while named.iter().any(|(taken, ..)| *taken == name) {
        name.push('_');
    }
    (named, Some((name, loose, true)))
}

/// A file of a result as a File or a Directory value; a relative path starts at `workdir`.
fn entry((path, directory): &File, workdir: &Path) -> Json {
    let location = match path.contains("://") {
        true => path.clone(),
        false => workdir.join(path).to_string_lossy().into_owned(),
    };
    let class = match directory {
        true => "Directory",
        false => "File",
    };
    let trimmed = location.trim_end_matches('/');
    let basename = trimmed.rsplit('/').next().unwrap_or(trimmed);
    json!({"class": class, "location": location, "basename": basename})
}

/// A file of a rule by its path, and whether `directory()` marks it.
fn file(interpreter: &mut Interpreter, value: &Value) -> Result<File, Error> {
    let directory = value.tag("directory").is_some();
    Ok((interpreter.to_str(value)?, directory))
}

/// The schema of an output: one file, or a list of files for a group of several.
fn output(files: &[File], many: bool) -> Json {
    let format = |directory: bool| match directory {
        true => DIRECTORY_PATH,
        false => FILE_PATH,
    };
    match (files, many) {
        ([(path, directory)], false) => {
            json!({"type": "string", "format": format(*directory), "const": path})
        }
        _ => {
            let directories = files.iter().all(|(_, directory)| *directory) && !files.is_empty();
            let paths: Vec<&str> = files.iter().map(|(path, _)| path.as_str()).collect();
            json!({
                "type": "array",
                "items": {"type": "string", "format": format(directories)},
                "const": paths
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_target_shapes() {
        let one = output(&[("results/report.html".into(), false)], false);
        assert_eq!(
            one,
            json!({"type": "string", "format": "file-path", "const": "results/report.html"})
        );
        let many = output(&[("a".into(), true), ("b".into(), true)], true);
        assert_eq!(many["items"]["format"], "directory-path");
        assert_eq!(many["const"], json!(["a", "b"]));
        assert_eq!(input_schema::valid(&many), Ok(()));
    }

    #[test]
    fn names_result_files() {
        let groups = vec![
            (Some("targets".into()), vec![("t.txt".into(), false)], false),
            (Some("reports".into()), vec![("r/a".into(), false)], true),
            (Some("reports".into()), vec![("x".into(), false)], false),
            (None, vec![("results/".into(), true)], false),
            (None, vec![("x".into(), false)], false),
        ];
        let values = results(groups, Path::new("/work"));
        let file = |location: &str, basename: &str| json!({"class": "File", "location": location, "basename": basename});
        let folder =
            json!({"class": "Directory", "location": "/work/results/", "basename": "results"});
        let expected = json!({
            "targets": file("/work/t.txt", "t.txt"),
            "reports": [file("/work/r/a", "a")],
            "targets_": [file("/work/x", "x"), folder],
        });
        assert_eq!(
            values, expected,
            "a taken name goes to the other files, once each"
        );
    }
}
