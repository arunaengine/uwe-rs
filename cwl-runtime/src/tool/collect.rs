//! Collecting the outputs of a finished task, as cwltool's `collect_output` does: from
//! `cwl.output.json`, or from the files that the globs of the output bindings match; in a mock
//! run, outputs the executor could not make get placeholders.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use cwl_parser::iri;
use cwl_parser::model::{Class, OutputBinding, OutputParameter, Type, short_name};
use workflow_graph::Outcome;

use super::{Collector, OUTPUT_JSON, Pending};
use crate::files;
use crate::placeholder;
use crate::value::{Map, Value};

impl Pending {
    /// Whether the task is an abstract `Operation`, which runs no tool.
    pub fn is_abstract(&self) -> bool {
        matches!(self.process.class, Class::Operation)
    }

    /// The output object of the task from its outcome.
    pub fn collect(self, outcome: &Outcome) -> Result<Map, String> {
        let exit = i64::from(outcome.exit_code);
        if exit != 0 && !self.success_codes.contains(&exit) {
            return Err(format!("the command ended with exit code {exit}"));
        }
        let mut runtime = self.runtime.clone();
        runtime.insert("exitCode".into(), Value::Integer(exit));
        let collector = Collector {
            pending: &self,
            outcome,
            runtime: Value::Map(runtime),
            access: self.options.access(),
            workdir: workdir(outcome),
        };
        let from_json = match outcome.files.iter().find(|file| file.path == OUTPUT_JSON) {
            Some(file) => {
                let text = std::fs::read_to_string(&file.location).unwrap_or_default();
                match Value::parse(&text) {
                    Ok(Value::Map(map)) => Some(map),
                    // The mock makes an empty file, which gives no outputs.
                    _ if self.options.mock => None,
                    Ok(_) => return Err(format!("{OUTPUT_JSON} must hold an object")),
                    Err(error) => return Err(format!("{OUTPUT_JSON}: {error}")),
                }
            }
            None => None,
        };
        let is_operation = !matches!(self.process.class, Class::CommandLineTool(_));
        let mut outputs = Map::new();
        if let Some(mut map) = from_json {
            for (_, value) in map.iter_mut() {
                collector.revmap_all(value)?;
            }
            let mut value = Value::Map(map);
            files::normalize(&mut value)?;
            let Value::Map(map) = value else {
                unreachable!("an object stays a map")
            };
            outputs = map;
            for (_, value) in outputs.iter_mut() {
                collector.checksums(value)?;
            }
        } else {
            for output in &self.process.outputs {
                let name = short_name(&output.id).to_owned();
                let collected = if is_operation {
                    collector.operation(output)
                } else if output.binding.is_none()
                    && !matches!(
                        self.types.resolve(&output.value_type),
                        Type::Stdout | Type::Stderr | Type::Record(_)
                    )
                {
                    // Only cwl.output.json gives this output; a mock run has none.
                    Ok(Value::Null)
                } else {
                    collector.output(
                        &name,
                        &output.value_type,
                        output.binding.as_ref(),
                        &output.secondary_files,
                        output.format.as_ref(),
                    )
                };
                let value = collected.map_err(|error| format!("the output `{name}`: {error}"))?;
                outputs.insert(name, value);
            }
        }
        for output in &self.process.outputs {
            let name = short_name(&output.id).to_owned();
            let value = outputs.get(&name).cloned().unwrap_or(Value::Null);
            if !self.types.matches(&output.value_type, &value) {
                if self.options.mock {
                    let placeholder = self.placeholder(output, &collector);
                    outputs.insert(name, placeholder);
                } else {
                    return Err(format!(
                        "the output `{name}` is of type {}, but its value is {}",
                        crate::inputs::describe(&output.value_type),
                        value.summary()
                    ));
                }
            }
        }
        for (_, value) in outputs.iter_mut() {
            files::visit_files(value, &mut |file| {
                file.shift_remove("path");
                file.shift_remove("dirname");
            });
        }
        Ok(outputs)
    }

    /// A value for an output the executor could not make, with files in its work folder.
    fn placeholder(&self, output: &OutputParameter, collector: &Collector<'_>) -> Value {
        let base = match &collector.workdir {
            Some(folder) => format!("{}/", iri::file_url(Path::new(folder))),
            None => "_:".to_owned(),
        };
        let name = short_name(&output.id);
        let hint = placeholder::hint_of(name, output.format.as_slice(), &output.secondary_files);
        let mut value = placeholder::value(&self.types, &output.value_type, name, &base, &hint);
        // Placeholder files point at the task's JSON output, so the graph keeps the link.
        if let Some(json) = collector
            .outcome
            .files
            .iter()
            .find(|file| file.path == OUTPUT_JSON)
        {
            let location = iri::file_url(&json.location);
            files::visit_files(&mut value, &mut |file| {
                if file.get("class").and_then(Value::as_str) == Some("File") {
                    let name = file
                        .get("location")
                        .and_then(Value::as_str)
                        .map(files::basename_of)
                        .unwrap_or_default();
                    file.insert("basename".into(), Value::String(name));
                    file.insert("location".into(), Value::String(location.clone()));
                }
            });
        }
        let _ = files::normalize(&mut value);
        let access = self.options.access();
        files::visit_files(&mut value, &mut |file| {
            if file.get("class").and_then(Value::as_str) == Some("File") {
                let _ = access.add_checksum(file);
            } else {
                access.load_listing(file, false);
            }
        });
        // The secondary files the output declares come with it, as with a real run.
        if !output.secondary_files.is_empty() {
            let _ = collector.secondary_files(&mut value, &output.secondary_files);
        }
        value
    }
}

/// The folder the executor put the task's outputs in, from any output file.
pub fn workdir(outcome: &Outcome) -> Option<String> {
    outcome.files.iter().find_map(|file| {
        let location = file.location.to_string_lossy();
        location
            .strip_suffix(file.path.trim_end_matches('/'))
            .map(|folder| folder.trim_end_matches('/').to_owned())
    })
}

impl Collector<'_> {
    /// The value of an output of an `Operation`: the file named after it, or a placeholder.
    fn operation(&self, output: &OutputParameter) -> Result<Value, String> {
        let name = short_name(&output.id);
        let types = &self.pending.types;
        let found = self.matches(name);
        let binding = OutputBinding {
            glob: Vec::new(),
            load_contents: None,
            load_listing: None,
            output_eval: None,
        };
        let mut file = match found.first() {
            Some(file) => self.file_object(file, &binding)?,
            None => return Ok(self.pending.placeholder(output, self)),
        };
        if !output.secondary_files.is_empty() {
            self.secondary_files(&mut file, &output.secondary_files)?;
        }
        let mut value_type = types.resolve(&output.value_type);
        let mut wrap = 0;
        loop {
            match value_type {
                Type::Array(array) => {
                    wrap += 1;
                    value_type = types.resolve(&array.items);
                }
                Type::Union(members) => {
                    match members.iter().find(|member| !matches!(member, Type::Null)) {
                        Some(member) => value_type = types.resolve(member),
                        None => break,
                    }
                }
                _ => break,
            }
        }
        let mut value = file;
        for _ in 0..wrap {
            value = Value::List(vec![value]);
        }
        let mut value = value;
        files::visit_files(&mut value, &mut |file| {
            file.shift_remove("path");
        });
        Ok(value)
    }
}
