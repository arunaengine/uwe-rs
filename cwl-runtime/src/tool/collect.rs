//! Collecting the outputs of a finished task, as cwltool's `collect_output` does: from
//! `cwl.output.json`, or from the files that the globs of the output bindings match; in a mock
//! run, outputs the executor could not make get placeholders.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use cwl_parser::iri;
use cwl_parser::model::{
    Class, Expression, OutputBinding, OutputParameter, SecondaryFile, Type, short_name,
};
use workflow_graph::Outcome;

use super::outputs::{classes, globs};
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

    /// cwltool's `collect_output` for one output or record field.
    fn output(
        &self,
        name: &str,
        value_type: &Type,
        binding: Option<&OutputBinding>,
        secondary: &[SecondaryFile],
        format: Option<&Expression>,
    ) -> Result<Value, String> {
        let types = &self.pending.types;
        let resolved = types.resolve(value_type);
        let binding = match (binding, resolved) {
            (Some(binding), _) => Some(binding.clone()),
            (None, Type::Stdout | Type::Stderr) => {
                let name = if matches!(resolved, Type::Stdout) {
                    &self.pending.stdout
                } else {
                    &self.pending.stderr
                };
                name.as_ref().map(|name| OutputBinding {
                    glob: vec![Expression {
                        text: name.clone(),
                        parts: vec![cwl_parser::model::Part::Text(name.clone())],
                    }],
                    load_contents: None,
                    load_listing: None,
                    output_eval: None,
                })
            }
            _ => None,
        };
        let Some(binding) = binding else {
            if let Type::Record(record) = resolved {
                let mut map = Map::new();
                for field in &record.fields {
                    let value = self.output(
                        &field.name,
                        &field.value_type,
                        field.output_binding.as_ref(),
                        &field.secondary_files,
                        field.format.first(),
                    )?;
                    map.insert(field.name.clone(), value);
                }
                return Ok(Value::Map(map));
            }
            return Ok(Value::Null);
        };
        let null = Value::Null;
        let patterns = globs(
            &binding,
            &self.pending.evaluator,
            self.vars(&null),
            &self.pending.outdir,
        )?;
        // Without `outputEval` the matches are the value, so they must be of a class the
        // output takes, as cwltool's type check of outputs finds.
        let (files, directories) = classes(types, value_type);
        let checked = binding.output_eval.is_none() && files != directories;
        let mut found: Vec<Value> = Vec::new();
        for pattern in &patterns {
            for file in self.matches(pattern) {
                if checked && file.directory != directories {
                    let (class, taken) = if file.directory {
                        ("directory", "files")
                    } else {
                        ("file", "directories")
                    };
                    return Err(format!(
                        "the glob `{pattern}` matches the {class} `{}`, but the output takes {taken}",
                        file.path
                    ));
                }
                self.check_link(&file)?;
                let mut object = self.file_object(&file, &binding)?;
                self.mock_extension(&mut object, &file, pattern, name, format, secondary);
                found.push(object);
            }
        }
        let (optional, single) = match resolved {
            Type::Union(members) => (
                members.contains(&Type::Null),
                members
                    .iter()
                    .any(|member| matches!(member, Type::File | Type::Directory)),
            ),
            Type::File | Type::Directory | Type::Stdout | Type::Stderr => (false, true),
            _ => (false, false),
        };
        let mut result = match &binding.output_eval {
            Some(expression) => match self.evaluate(expression, &Value::List(found)) {
                Ok(value) => value,
                // Expressions that read the files of a mock run read empty files and may
                // fail; the output then gets a placeholder, as when it does not fit its type.
                Err(_) if self.access.assume_present && reads_self(expression) => Value::Null,
                Err(error) => return Err(error),
            },
            None => Value::List(found),
        };
        if single {
            match result {
                Value::List(items) if items.is_empty() => {
                    if !optional && !self.access.assume_present {
                        return Err(format!("no output file matches {patterns:?}"));
                    }
                    result = Value::Null;
                }
                Value::List(mut items) if items.len() == 1 => result = items.remove(0),
                Value::List(items) if !self.access.assume_present => {
                    return Err(format!(
                        "{} files match {patterns:?}, where one is expected",
                        items.len()
                    ));
                }
                Value::List(mut items) => result = items.remove(0),
                other => result = other,
            }
        }
        if !secondary.is_empty() {
            self.secondary_files(&mut result, secondary)?;
        }
        if let Some(format) = format {
            let mut primaries: Vec<&mut Value> = match &mut result {
                Value::List(items) => items.iter_mut().collect(),
                other => vec![other],
            };
            for primary in primaries.iter_mut() {
                if !primary.is_file() {
                    continue;
                }
                let value = match self.evaluate(format, primary) {
                    Ok(value) => value,
                    // The format is metadata; over the inputs of a mock run it may not exist.
                    Err(_) if self.access.assume_present => Value::Null,
                    Err(error) => return Err(error),
                };
                if let (Value::Map(map), Value::String(format)) = (&mut **primary, value) {
                    map.insert("format".into(), Value::String(format));
                }
            }
        }
        self.revmap_all(&mut result)?;
        // cwltool keeps 0 and "" but turns other empty results of optional outputs into null.
        let kept = matches!(
            result,
            Value::Integer(0) | Value::Bool(false) | Value::String(_)
        ) || matches!(result, Value::Float(number) if number == 0.0);
        if optional && !result.is_truthy() && !kept {
            return Ok(Value::Null);
        }
        Ok(result)
    }
}

/// Whether an expression reads `self`, the files an output binding found.
fn reads_self(expression: &Expression) -> bool {
    expression.parts.iter().any(|part| match part {
        cwl_parser::model::Part::Text(_) => false,
        cwl_parser::model::Part::Reference(code)
        | cwl_parser::model::Part::Javascript(code)
        | cwl_parser::model::Part::Function(code) => code.contains("self"),
    })
}
