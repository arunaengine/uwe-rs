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
}
