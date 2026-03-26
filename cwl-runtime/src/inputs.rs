//! The input object of a process, as cwltool's `_init_job` makes it: defaults for missing
//! inputs, file objects with their derived fields, listings, sizes, contents and secondary
//! files, and a check of every value against its type.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use cwl_parser::model::{
    Expression, InputParameter, LoadListing, Process, SecondaryFile, Type, Version, short_name,
};

use crate::expression::{Evaluator, Variables};
use crate::files::{self, Access};
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
    /// for the inputs of the process a run starts with.
    pub discover: bool,
    /// In a run on placeholder inputs, the folder URL of placeholder files: a required input
    /// that gets null, as optional inputs are null there, gets a placeholder too.
    pub placeholders: Option<&'a str>,
}

/// Fills in and checks the input object of a process.
pub fn prepare(process: &Process, mut job: Map, setting: &Setting<'_>) -> Result<Map, String> {
    for input in &process.inputs {
        let name = short_name(&input.id).to_owned();
        let present = job.get(&name).is_some_and(|value| !value.is_null());
        if present {
            continue;
        }
        let value = if let Some(default) = &input.default {
            Value::from_node(default)
        } else if setting.types.optional(&input.value_type) {
            Value::Null
        } else if let Some(base) = setting.placeholders {
            placeholder::input(input, setting.types, base, false)
        } else {
            return Err(format!("the required input `{name}` has no value"));
        };
        job.insert(name, value);
    }
    let mut value = Value::Map(job);
    files::normalize(&mut value)?;
    let Value::Map(mut job) = value else {
        unreachable!("the job stays a map");
    };
    for input in &process.inputs {
        let name = short_name(&input.id);
        let value = job.get(name).unwrap_or(&Value::Null);
        if !setting.types.matches(&input.value_type, value) {
            return Err(format!(
                "the input `{name}` is not a valid {}: {}",
                describe(&input.value_type),
                value.to_json()
            ));
        }
    }
    let deep = setting.load_listing == Some(LoadListing::DeepListing)
        || (setting.load_listing.is_none() && process.version == Version::V1_0);
    let shallow = setting.load_listing == Some(LoadListing::ShallowListing);
    if deep || shallow {
        for (_, value) in job.iter_mut() {
            setting.access.load_listings(value, deep);
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
            legacy: process.version == Version::V1_0,
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

/// Walks a value along its type to load contents, listings and secondary files where the
/// schema asks for them.
struct Walk<'a, 'b> {
    setting: &'a Setting<'b>,
    inputs: &'a Value,
    legacy: bool,
}

/// What a schema asks of the files at one place.
#[derive(Clone, Copy)]
struct Asks<'a> {
    secondary: &'a [SecondaryFile],
    load_contents: bool,
    /// Whether array items load their contents too, as a binding's `loadContents` does.
    items_load: bool,
    load_listing: Option<LoadListing>,
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
            items_load: binding_loads || (self.legacy && schema_loads),
            load_listing: input.load_listing,
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
                        .load_listing(directory, listing == LoadListing::DeepListing);
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
        if asks.secondary.is_empty() {
            return Ok(());
        }
        let trusted = file.contains_key("secondaryFiles") && !self.setting.discover;
        if trusted {
            return Ok(());
        }
        if !file.contains_key("secondaryFiles") {
            file.insert("secondaryFiles".into(), Value::List(Vec::new()));
        }
        for pattern in asks.secondary {
            let this = Value::Map(file.clone());
            let required = match &pattern.required {
                None => true,
                Some(cwl_parser::model::Dynamic::Value(required)) => *required,
                Some(cwl_parser::model::Dynamic::Expression(expression)) => {
                    self.evaluate(expression, &this)?.is_truthy()
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
        if given || (self.setting.discover && self.setting.access.exists(&entry_location)) {
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
