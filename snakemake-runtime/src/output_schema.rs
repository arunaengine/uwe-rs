//! The outputs of a Snakemake workflow as one JSON Schema (decision 24): the files of its target
//! rules, which are the first rule unless the launch names targets. A rule with outputs gives
//! them; a rule without, like `all`, gives the inputs that it asks for. A named group of files is
//! an output by that name, and the other files are the output `targets`. Each output holds the
//! files for the config of the launch, as `const`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use input_schema::{DIRECTORY_PATH, DRAFT, FILE_PATH};
use python_evaluator::{Error, Interpreter, Value};
use serde_json::{Map, Value as Json, json};

use crate::pattern::Wildcards;
use crate::rule::Rule;

/// A file of an output, and whether it is a directory.
type File = (String, bool);

/// The schema of the files of the target rules or files.
pub fn schema(
    interpreter: &mut Interpreter,
    rules: &[Rule],
    targets: &[String],
    title: &str,
) -> Result<Json, Error> {
    let mut properties = Map::new();
    let mut loose: Vec<File> = Vec::new();
    for target in targets {
        let Some(rule) = rules.iter().find(|rule| rule.name == *target) else {
            loose.push((target.clone(), false));
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
            match name {
                Some(name) if !properties.contains_key(&name) => {
                    properties.insert(name, output(&files, many));
                }
                _ => loose.extend(files),
            }
        }
    }
    // A file that several target rules ask for is one output.
    let mut seen = std::collections::HashSet::new();
    loose.retain(|(path, _)| seen.insert(path.clone()));
    if !loose.is_empty() && !properties.contains_key("targets") {
        let mut schema = output(&loose, true);
        schema["description"] = json!("the files that the target rules make or ask for");
        properties.insert("targets".into(), schema);
    }
    Ok(json!({"$schema": DRAFT, "title": title, "type": "object", "properties": properties}))
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
}
