//! What the tests of the test workflows record of the inputs of a workflow: the size of its
//! schema, whether the schema is valid, the problems of a check, and where the schema and the
//! code differ, on one line.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt;

use serde_json::Value as Json;
use workflow_graph::Inputs;

use crate::check::{Severity, check};
use crate::{DIRECTORY_PATH, FILE_PATH, FILE_PATH_PATTERN, PATH, properties, valid};

/// The counts of the inputs of a workflow.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Counts {
    /// The inputs at the top of the schema.
    pub inputs: usize,
    pub required: usize,
    /// The files and directories at any depth of the inputs, outside the content of files.
    pub files: usize,
    /// The inputs whose content has a schema, like sample sheets.
    pub tables: usize,
    /// Why the schema is not valid JSON Schema, if it is not.
    pub invalid: Option<String>,
    /// The errors and warnings of a check of the values of the run.
    pub errors: usize,
    pub warnings: usize,
    /// The inputs that the code reads and the schema does not declare, and the other way round.
    pub undeclared: usize,
    pub unread: usize,
}

impl Counts {
    /// The counts of the inputs of a run, simulated or not.
    pub fn of(inputs: &Inputs, simulated: bool) -> Counts {
        let top = properties(&inputs.schema);
        let problems = check(inputs, simulated);
        let severity = |wanted| {
            problems
                .iter()
                .filter(|problem| problem.severity == wanted)
                .count()
        };
        let (mut undeclared, mut unread) = (0, 0);
        if let Some(read) = &inputs.read {
            let declared: Vec<&str> = top.iter().map(|property| property.name).collect();
            undeclared = read
                .iter()
                .filter(|name| !declared.contains(&name.as_str()))
                .count();
            unread = declared
                .iter()
                .filter(|name| !read.contains(**name))
                .count();
        }
        Counts {
            inputs: top.len(),
            required: top.iter().filter(|property| property.required).count(),
            files: top.iter().map(|property| files(property.schema)).sum(),
            tables: top
                .iter()
                .filter(|property| property.schema.get("schema").is_some_and(Json::is_object))
                .count(),
            invalid: valid(&inputs.schema).err(),
            errors: severity(Severity::Error),
            warnings: severity(Severity::Warning),
            undeclared,
            unread,
        }
    }
}

/// The files and directories that a schema describes: itself, the properties of an object and
/// the items of a list, but not the content of a file.
fn files(schema: &Json) -> usize {
    let own = match schema.get("format").and_then(Json::as_str) {
        Some(FILE_PATH | DIRECTORY_PATH | PATH | FILE_PATH_PATTERN) => 1,
        _ => 0,
    };
    let properties: usize = match schema.get("properties") {
        Some(Json::Object(properties)) => properties.values().map(files).sum(),
        _ => 0,
    };
    let items = schema.get("items").map_or(0, files);
    own + properties + items
}

impl fmt::Display for Counts {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(reason) = &self.invalid {
            let reason: String = reason.replace('\n', " ").chars().take(200).collect();
            return write!(formatter, "invalid schema: {reason}");
        }
        write!(
            formatter,
            "ok: {} inputs, {} required, {} files, {} tables; {} errors, {} warnings; \
             {} read not declared, {} declared not read",
            self.inputs,
            self.required,
            self.files,
            self.tables,
            self.errors,
            self.warnings,
            self.undeclared,
            self.unread
        )
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn counts_of_a_schema() {
        let inputs = Inputs {
            schema: json!({
                "type": "object",
                "required": ["input"],
                "properties": {
                    "input": {"type": "string", "format": "file-path", "schema": {"type": "array"}},
                    "reads": {"type": "array", "items": {"type": "string", "format": "file-path"}},
                    "threads": {"type": "integer"}
                }
            }),
            values: json!({"threads": "x"}),
            read: Some(["input", "genome"].map(str::to_owned).into()),
            ..Inputs::default()
        };
        assert_eq!(
            Counts::of(&inputs, true).to_string(),
            "ok: 3 inputs, 1 required, 2 files, 1 tables; 2 errors, 0 warnings; 1 read not \
             declared, 2 declared not read"
        );
    }
}
