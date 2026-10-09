//! Builds process inputs with defaults, file metadata, listings and secondary files.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashSet;

use cwl_parser::model::{
    Expression, InputParameter, LoadListing, Process, SecondaryFile, Type, short_name,
};

use crate::expression::{Evaluator, Variables};
use crate::files::{self, Access};
use crate::ontology::Ontology;
use crate::placeholder;
use crate::types::Types;
use crate::value::{Map, Value};

/// What preparing an input object needs besides the process.
pub struct Setting<'a> {
    pub types: &'a Types,
    pub evaluator: &'a Evaluator,
    pub access: Access,
    /// The `LoadListingRequirement` in effect.
    pub load_listing: Option<LoadListing>,
    /// Whether secondary files are looked for next to their primary file, which cwltool does
    /// for the inputs of the process a run starts with only; inside a workflow they come with
    /// the values.
    pub discover: bool,
    /// In a run on placeholder inputs, the folder URL of placeholder files: a required input
    /// that gets null, as optional inputs are null there, gets a placeholder too.
    pub placeholders: Option<&'a str>,
    /// Whether the process is the one the run starts with. On placeholder inputs, its defaults
    /// that do not fit their type give way to placeholders, as a real job has to give them.
    pub toplevel: bool,
    /// Whether the optional inputs of the process the run starts with get placeholders too,
    /// and its flags are on.
    pub fill_optional: bool,
    /// The strings that the placeholders of the top process hold, which give way to
    /// placeholders where they do not fit.
    pub placeholder_strings: &'a HashSet<String>,
    /// Location prefixes of files that have the formats and secondary files asked of them, as
    /// a job that works gives them: placeholder files, and the outputs of abstract steps.
    pub assumed: &'a [String],
    pub ontologies: &'a Ontology,
}

/// Fills in and checks the input object of a process.
pub fn prepare(process: &Process, mut job: Map, setting: &Setting<'_>) -> Result<Map, String> {
    let mut defaulted = Vec::new();
    for input in &process.inputs {
        let name = short_name(&input.id).to_owned();
        let present = job.get(&name).is_some_and(|value| !value.is_null());
        if present {
            continue;
        }
        let fill = setting.toplevel && setting.fill_optional;
        let placeholders = setting
            .placeholders
            .filter(|_| fill || !setting.types.optional(&input.value_type));
        let value = if let Some(default) = &input.default {
            defaulted.push(name.clone());
            Value::from_node(default)
        } else if let Some(base) = placeholders {
            // A job that sets every input also switches its flags on.
            match placeholder::input(input, setting.types, base) {
                Value::Bool(false) if fill => Value::Bool(true),
                value => value,
            }
        } else if setting.types.optional(&input.value_type) {
            Value::Null
        } else {
            return Err(format!("the required input `{name}` has no value"));
        };
        job.insert(name, value);
    }
    for (name, value) in job.iter_mut() {
        let Err(error) = files::normalize(value) else {
            continue;
        };
        // On placeholder inputs, a default that is not a valid file object gives way to a
        // placeholder, as a real job has to give that input.
        let input = process
            .inputs
            .iter()
            .find(|input| short_name(&input.id) == name);
        match (setting.placeholders, input) {
            (Some(base), Some(input)) if setting.toplevel && defaulted.contains(name) => {
                *value = placeholder::input(input, setting.types, base);
                files::normalize(value)?;
            }
            _ => return Err(format!("the input `{name}`: {error}")),
        }
    }
    for input in &process.inputs {
        let name = short_name(&input.id);
        let value = job.get(name).unwrap_or(&Value::Null);
        let replaceable = holds_null(value)
            || (setting.toplevel && defaulted.iter().any(|defaulted| defaulted == name))
            || placeholder::is_placeholder_string(value, setting.placeholder_strings);
        if !setting.types.matches(&input.value_type, value)
            && let Some(base) = setting.placeholders
            && replaceable
        {
            // Nulls of optional inputs of a run on placeholder inputs, merged into a list,
            // defaults of the wrong type, and placeholder strings that reach an enum.
            let mut value = placeholder::input(input, setting.types, base);
            files::normalize(&mut value)?;
            job.insert(name.to_owned(), value);
            continue;
        }
        if !setting.types.matches(&input.value_type, value) {
            return Err(format!(
                "the input `{name}` is of type {}, but its value is {}",
                describe(&input.value_type),
                value.summary()
            ));
        }
    }
    if setting.toplevel {
        absent(&mut job, setting)?;
    }
    let deep = setting.load_listing == Some(LoadListing::DeepListing);
    let shallow = setting.load_listing == Some(LoadListing::ShallowListing);
    if deep || shallow {
        for (_, value) in job.iter_mut() {
            setting.access.load_listings(value, deep)?;
        }
    }
    let snapshot = Value::Map(job.clone());
    for input in &process.inputs {
        let name = short_name(&input.id);
        let Some(value) = job.get_mut(name) else {
            continue;
        };
        let mut walk = Walk {
            setting,
            inputs: &snapshot,
        };
        walk.parameter(input, value)?;
    }
    for (_, value) in job.iter_mut() {
        files::visit_files(value, &mut |file| {
            if file.get("class").and_then(Value::as_str) == Some("File") {
                setting.access.add_size(file);
            }
        });
    }
    Ok(job)
}

/// Fails on the first file or directory of the input object that does not exist, as a run
/// needs them and cwltool fails on them when it stages them. A run that assumes its inputs
/// takes every file as present, and placeholders are never on disk.
fn absent(job: &mut Map, setting: &Setting<'_>) -> Result<(), String> {
    for (name, value) in job.iter_mut() {
        let mut missing = None;
        files::visit_files(value, &mut |file| {
            if let Some(location) = file.get("location").and_then(Value::as_str)
                && missing.is_none()
                && !setting
                    .assumed
                    .iter()
                    .any(|prefix| location.starts_with(prefix))
                && !setting.access.exists(location)
            {
                missing = Some(
                    files::local_path(location)
                        .map_or_else(|| location.to_owned(), |path| path.display().to_string()),
                );
            }
        });
        if let Some(path) = missing {
            return Err(format!(
                "the input `{name}` names `{path}`, which does not exist"
            ));
        }
    }
    Ok(())
}

/// Walks a value along its type to load contents, listings and secondary files where the
/// schema asks for them.
struct Walk<'a, 'b> {
    setting: &'a Setting<'b>,
    inputs: &'a Value,
}

/// What a schema asks of the files at one place.
#[derive(Clone, Copy)]
struct Asks<'a> {
    secondary: &'a [SecondaryFile],
    load_contents: bool,
    /// Whether array items load their contents too, as a binding's `loadContents` does.
    items_load: bool,
    load_listing: Option<LoadListing>,
    /// The formats a file may have.
    format: &'a [Expression],
}

impl Walk<'_, '_> {
    fn parameter(&mut self, input: &InputParameter, value: &mut Value) -> Result<(), String> {
        let binding_loads = input
            .binding
            .as_ref()
            .and_then(|binding| binding.load_contents)
            .unwrap_or(false);
        let schema_loads = input.load_contents.unwrap_or(false);
        let asks = Asks {
            secondary: &input.secondary_files,
            load_contents: binding_loads || schema_loads,
            items_load: binding_loads || schema_loads,
            load_listing: input.load_listing,
            format: &input.format,
        };
        self.value(&input.value_type, asks, value)
    }

    fn value(
        &mut self,
        value_type: &Type,
        asks: Asks<'_>,
        value: &mut Value,
    ) -> Result<(), String> {
        if value.is_null() {
            return Ok(());
        }
        let types = self.setting.types;
        match types.resolve(value_type) {
            Type::Union(members) => {
                let member = members.iter().find(|member| types.matches(member, value));
                if let Some(member) = member {
                    return self.value(member, asks, value);
                }
            }
            Type::Array(array) => {
                if let Value::List(items) = value {
                    let item_asks = Asks {
                        load_contents: asks.items_load,
                        ..asks
                    };
                    for item in items {
                        self.value(&array.items, item_asks, item)?;
                    }
                }
            }
            Type::Record(record) => {
                if let Value::Map(map) = value {
                    for field in &record.fields {
                        let Some(item) = map.get_mut(&field.name) else {
                            continue;
                        };
                        let loads = field
                            .input_binding
                            .as_ref()
                            .and_then(|binding| binding.load_contents)
                            .or(field.load_contents)
                            .unwrap_or(false);
                        let field_asks = Asks {
                            secondary: &field.secondary_files,
                            load_contents: loads,
                            items_load: loads,
                            load_listing: field.load_listing,
                            format: &field.format,
                        };
                        self.value(&field.value_type, field_asks, item)?;
                    }
                }
            }
            Type::File | Type::Stdin => {
                if let Value::Map(file) = value {
                    self.file(file, asks)?;
                }
            }
            Type::Directory => {
                if let Value::Map(directory) = value
                    && let Some(listing) = asks.load_listing
                    && listing != LoadListing::NoListing
                {
                    self.setting
                        .access
                        .load_listing(directory, listing == LoadListing::DeepListing)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn file(&mut self, file: &mut Map, asks: Asks<'_>) -> Result<(), String> {
        if asks.load_contents {
            self.setting.access.load_contents(file)?;
        }
        if !asks.secondary.is_empty() {
            self.secondary_files(file, asks.secondary)?;
        }
        if !asks.format.is_empty() {
            self.check_format(file, asks.format)?;
        }
        Ok(())
    }

    /// Fails when a file has no format, or one that none of the declared formats is, as
    /// cwltool's `check_format`. An assumed file without a format takes the first one, as a
    /// job that works gives it.
    fn check_format(&self, file: &mut Map, declared: &[Expression]) -> Result<(), String> {
        let mut allowed = Vec::new();
        for expression in declared {
            match self.evaluate(expression, &Value::Null)? {
                Value::String(format) => allowed.push(format),
                Value::List(items) => {
                    for item in items {
                        let Value::String(format) = item else {
                            return Err(format!(
                                "a format must be a string, not {}",
                                item.to_json()
                            ));
                        };
                        allowed.push(format);
                    }
                }
                other => {
                    return Err(format!(
                        "a format must be a string or a list of strings, not {}",
                        other.to_json()
                    ));
                }
            }
        }
        let location = file.get("location").and_then(Value::as_str).unwrap_or("");
        let placeholder = self
            .setting
            .assumed
            .iter()
            .any(|prefix| location.starts_with(prefix.as_str()));
        if placeholder
            && !file.contains_key("format")
            && let Some(first) = allowed.first()
        {
            file.insert("format".into(), Value::String(first.clone()));
        }
        let name = file.get("basename").and_then(Value::as_str).unwrap_or("");
        let Some(format) = file.get("format").and_then(Value::as_str) else {
            return Err(format!("the file `{name}` has no format, but needs one"));
        };
        if self.setting.ontologies.matches(format, &allowed)? {
            return Ok(());
        }
        Err(format!(
            "the file `{name}` has the format `{format}`, not one of `{}`",
            allowed.join("`, `")
        ))
    }

    /// Adds the secondary files of a file that the patterns name.
    fn secondary_files(
        &mut self,
        file: &mut Map,
        patterns: &[SecondaryFile],
    ) -> Result<(), String> {
        let trusted = file.contains_key("secondaryFiles") && !self.setting.discover;
        if trusted {
            return Ok(());
        }
        if !file.contains_key("secondaryFiles") {
            file.insert("secondaryFiles".into(), Value::List(Vec::new()));
        }
        for pattern in patterns {
            let this = Value::Map(file.clone());
            let required = match &pattern.required {
                None => true,
                Some(cwl_parser::model::Dynamic::Value(required)) => *required,
                Some(cwl_parser::model::Dynamic::Expression(expression)) => {
                    match self.evaluate(expression, &this)? {
                        Value::Bool(required) => required,
                        // An optional boolean input that is not given, as cwltool reads it.
                        Value::Null => false,
                        other => {
                            return Err(format!(
                                "secondaryFiles.required must give a boolean, not {}",
                                other.to_json()
                            ));
                        }
                    }
                }
            };
            let names = if has_code(&pattern.pattern) {
                self.evaluate(&pattern.pattern, &this)?
            } else {
                let basename = file
                    .get("basename")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                Value::String(files::substitute(basename, &pattern.pattern.text))
            };
            let names = match names {
                Value::List(items) => items,
                Value::Null => Vec::new(),
                other => vec![other],
            };
            for name in names {
                self.secondary(file, name, required)?;
            }
        }
        let mut list = file
            .shift_remove("secondaryFiles")
            .unwrap_or(Value::List(Vec::new()));
        files::normalize(&mut list)?;
        file.insert("secondaryFiles".into(), list);
        Ok(())
    }

    /// Adds one secondary file, named by a string or given as a file object.
    fn secondary(&mut self, file: &mut Map, name: Value, required: bool) -> Result<(), String> {
        let location = file
            .get("location")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let (entry, basename, given) = match name {
            Value::String(name) if name.is_empty() => return Ok(()),
            Value::String(name) => {
                let folder = match location.rfind('/') {
                    Some(slash) => &location[..=slash],
                    None => location.as_str(),
                };
                let mut entry = Map::new();
                entry.insert("location".into(), Value::String(format!("{folder}{name}")));
                entry.insert("basename".into(), Value::String(name.clone()));
                entry.insert("class".into(), Value::string("File"));
                (entry, name, false)
            }
            Value::Map(entry) => {
                let basename = entry
                    .get("basename")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| {
                        files::basename_of(
                            entry.get("location").and_then(Value::as_str).unwrap_or(""),
                        )
                    });
                (entry, basename, true)
            }
            other => {
                return Err(format!(
                    "a secondaryFiles expression gave {}, not a name or a file",
                    other.to_json()
                ));
            }
        };
        let Some(Value::List(list)) = file.get_mut("secondaryFiles") else {
            return Ok(());
        };
        let found = list.iter().any(|existing| {
            let name = existing
                .text("basename")
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    files::basename_of(existing.text("location").unwrap_or_default())
                });
            name == basename
        });
        if found {
            return Ok(());
        }
        let entry_location = entry
            .get("location")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let found = self
            .setting
            .access
            .exists_next_to(&entry_location, &location);
        // An assumed file has the secondary files asked of it, as a job that works lists them.
        let placeholder = self
            .setting
            .assumed
            .iter()
            .any(|prefix| location.starts_with(prefix.as_str()));
        if given || ((self.setting.discover || placeholder) && found) {
            add_secondary(list, entry);
        } else if required {
            return Err(format!(
                "the required secondary file `{basename}` of `{location}` is missing"
            ));
        }
        Ok(())
    }

    fn evaluate(&self, expression: &Expression, this: &Value) -> Result<Value, String> {
        let runtime = Value::Map(Map::new());
        self.setting.evaluator.evaluate(
            expression,
            Variables {
                inputs: self.inputs,
                this,
                runtime: &runtime,
            },
        )
    }
}

/// Whether a value is null or holds a null.
fn holds_null(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::List(items) => items.iter().any(holds_null),
        Value::Map(map) => map.values().any(holds_null),
        _ => false,
    }
}

/// Whether an expression string holds code rather than only text.
pub fn has_code(expression: &Expression) -> bool {
    expression
        .parts
        .iter()
        .any(|part| !matches!(part, cwl_parser::model::Part::Text(_)))
}

/// Adds a secondary file unless one with its location is there; then only its name changes.
fn add_secondary(list: &mut Vec<Value>, entry: Map) {
    let location = entry.get("location").cloned();
    for existing in list.iter_mut() {
        if let Value::Map(existing) = existing
            && existing.get("location") == location.as_ref()
        {
            if let Some(basename) = entry.get("basename") {
                existing.insert("basename".into(), basename.clone());
            }
            return;
        }
    }
    list.push(Value::Map(entry));
}

/// A type for messages, like `File[]` or `string?`.
pub fn describe(value_type: &Type) -> String {
    match value_type {
        Type::Null => "null".into(),
        Type::Boolean => "boolean".into(),
        Type::Int => "int".into(),
        Type::Long => "long".into(),
        Type::Float => "float".into(),
        Type::Double => "double".into(),
        Type::String => "string".into(),
        Type::File | Type::Stdin | Type::Stdout | Type::Stderr => "File".into(),
        Type::Directory => "Directory".into(),
        Type::Any => "Any".into(),
        Type::Array(array) => format!("{}[]", describe(&array.items)),
        Type::Record(record) => match &record.name {
            Some(name) => short_name(name).to_owned(),
            None => "record".into(),
        },
        Type::Enum(schema) => match &schema.name {
            Some(name) => short_name(name).to_owned(),
            None => "enum".into(),
        },
        Type::Union(members) => {
            let others: Vec<String> = members
                .iter()
                .filter(|member| !matches!(member, Type::Null))
                .map(describe)
                .collect();
            let optional = members.contains(&Type::Null);
            format!("{}{}", others.join(" or "), if optional { "?" } else { "" })
        }
        Type::Named(name) => short_name(name).to_owned(),
    }
}

#[cfg(test)]
#[path = "inputs_tests.rs"]
mod tests;
