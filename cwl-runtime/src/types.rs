//! Types of values: named types from `SchemaDefRequirement`, checks of values against types,
//! and placeholder values for inputs and outputs that nothing gives.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;

use cwl_parser::model::{Type, short_name};

use crate::value::{Map, Value};

/// The named types in effect for a process.
#[derive(Clone, Debug, Default)]
pub struct Types {
    named: HashMap<String, Type>,
}

impl Types {
    /// Registers a type and every named type inside it.
    pub fn add(&mut self, value_type: &Type) {
        let name = match value_type {
            Type::Record(record) => {
                for field in &record.fields {
                    self.add(&field.value_type);
                }
                record.name.clone()
            }
            Type::Enum(schema) => schema.name.clone(),
            Type::Array(array) => {
                self.add(&array.items);
                array.name.clone()
            }
            Type::Union(types) => {
                for member in types {
                    self.add(member);
                }
                None
            }
            _ => None,
        };
        if let Some(name) = name {
            self.named.entry(name).or_insert_with(|| value_type.clone());
        }
    }

    /// The type a name stands for, followed until it is not a name. A name that no schema
    /// defines stays as it is.
    pub fn resolve<'t>(&'t self, value_type: &'t Type) -> &'t Type {
        let mut current = value_type;
        for _ in 0..16 {
            let Type::Named(name) = current else {
                return current;
            };
            let found = self.named.get(name).or_else(|| {
                let short = fragment(name);
                self.named
                    .iter()
                    .find(|(key, _)| fragment(key) == short)
                    .map(|(_, value_type)| value_type)
            });
            match found {
                Some(next) => current = next,
                None => return current,
            }
        }
        current
    }

    /// Whether a value fits a type, as Schema Salad validates job objects: numbers fit float
    /// types, records may have extra fields, and names are looked up.
    pub fn matches(&self, value_type: &Type, value: &Value) -> bool {
        match (self.resolve(value_type), value) {
            (Type::Null, Value::Null) => true,
            (Type::Boolean, Value::Bool(_)) => true,
            // Documents written for other runners pass 64-bit values to `int`, which is harmless.
            (Type::Int | Type::Long, Value::Integer(_)) => true,
            (Type::Float | Type::Double, value) => value.is_number(),
            (Type::String, Value::String(_)) => true,
            (Type::File | Type::Stdin | Type::Stdout | Type::Stderr, value) => value.is_file(),
            (Type::Directory, value) => value.is_directory(),
            (Type::Any, value) => !value.is_null(),
            (Type::Array(array), Value::List(items)) => {
                items.iter().all(|item| self.matches(&array.items, item))
            }
            (Type::Record(record), Value::Map(map)) => record.fields.iter().all(|field| {
                let value = map.get(&field.name).unwrap_or(&Value::Null);
                self.matches(&field.value_type, value)
            }),
            (Type::Enum(schema), Value::String(text)) => schema
                .symbols
                .iter()
                .any(|symbol| symbol == text || symbol == short_name(text)),
            (Type::Union(types), value) => types.iter().any(|member| self.matches(member, value)),
            // A type no schema defines cannot be checked.
            (Type::Named(_), _) => true,
            _ => false,
        }
    }

    /// Whether a parameter may be left out: its type is null or a union with null.
    pub fn optional(&self, value_type: &Type) -> bool {
        match self.resolve(value_type) {
            Type::Null => true,
            Type::Union(types) => types.iter().any(|member| matches!(member, Type::Null)),
            _ => false,
        }
    }

    /// A value of the type for a parameter that nothing sets: a file named after the
    /// parameter in the folder `base` (a URL ending in `/`), one item for arrays, the first
    /// symbol of enums. `null_if_optional` gives null for optional types.
    pub fn placeholder(
        &self,
        value_type: &Type,
        name: &str,
        base: &str,
        null_if_optional: bool,
        hint: &Hint,
    ) -> Value {
        match self.resolve(value_type) {
            Type::Null => Value::Null,
            Type::Boolean => Value::Bool(false),
            Type::Int | Type::Long => Value::Integer(1),
            Type::Float | Type::Double => Value::Float(1.0),
            Type::String | Type::Any | Type::Named(_) => Value::string(name),
            Type::File | Type::Stdin | Type::Stdout | Type::Stderr => {
                let mut file = file_object("File", &format!("{name}{}", hint.extension), base);
                if let (Value::Map(map), Some(format)) = (&mut file, &hint.format) {
                    map.insert("format".into(), Value::String(format.clone()));
                }
                file
            }
            Type::Directory => {
                // One file inside, as expressions often look at the first entry.
                let mut directory = file_object("Directory", name, base);
                let folder = format!("{}/", directory.text("location").unwrap_or(base));
                let inside = file_object("File", &format!("{name}.txt"), &folder);
                if let Value::Map(map) = &mut directory {
                    map.insert("basename".into(), Value::string(name));
                    map.insert("listing".into(), Value::List(vec![inside]));
                }
                directory
            }
            Type::Array(array) => Value::List(vec![self.placeholder(
                &array.items,
                name,
                base,
                false,
                hint,
            )]),
            Type::Record(record) => {
                let mut map = Map::new();
                for field in &record.fields {
                    let value = self.placeholder(
                        &field.value_type,
                        &field.name,
                        base,
                        null_if_optional,
                        &Hint::default(),
                    );
                    map.insert(field.name.clone(), value);
                }
                Value::Map(map)
            }
            Type::Enum(schema) => schema.symbols.first().map_or(Value::Null, Value::string),
            Type::Union(types) => {
                if null_if_optional && types.contains(&Type::Null) {
                    return Value::Null;
                }
                // Files, as expressions over such unions mostly look at file fields.
                let file = types
                    .iter()
                    .find(|member| matches!(self.resolve(member), Type::File | Type::Directory));
                let first = types.iter().find(|member| !matches!(member, Type::Null));
                match file.or(first) {
                    Some(member) => self.placeholder(member, name, base, null_if_optional, hint),
                    None => Value::Null,
                }
            }
        }
    }
}

/// What a placeholder file looks like: the extension of its name and its format.
#[derive(Clone, Debug, Default)]
pub struct Hint {
    pub extension: String,
    pub format: Option<String>,
}

/// The last `#` segment of a name, or the name.
fn fragment(name: &str) -> &str {
    name.rsplit_once('#').map_or(name, |(_, fragment)| fragment)
}

fn file_object(class: &str, name: &str, base: &str) -> Value {
    let mut map = Map::new();
    map.insert("class".into(), Value::string(class));
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    map.insert("location".into(), Value::String(format!("{base}{safe}")));
    Value::Map(map)
}
