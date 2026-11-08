//! The wir types of CWL parameter types.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use cwl_parser::model::{RequirementClass, Type as Cwl, short_name};
use wir::{ArrayType, Field, FileType, Type};

use crate::Scope;

/// How deep named types may refer to other named types; CWL types do not refer to themselves.
const DEPTH: usize = 32;

/// The named types in scope: those of `SchemaDefRequirement`, with the named types in them.
pub fn named(scope: &Scope) -> Vec<Cwl> {
    let mut found = Vec::new();
    for entry in scope.requirements.iter().chain(&scope.hints) {
        if let RequirementClass::SchemaDef { types } = &entry.class {
            types.iter().for_each(|item| collect(item, &mut found));
        }
    }
    found
}

fn collect(cwl: &Cwl, found: &mut Vec<Cwl>) {
    let name = match cwl {
        Cwl::Record(record) => {
            (record.fields.iter()).for_each(|field| collect(&field.value_type, found));
            &record.name
        }
        Cwl::Enum(schema) => &schema.name,
        Cwl::Array(array) => {
            collect(&array.items, found);
            &array.name
        }
        Cwl::Union(members) => {
            members.iter().for_each(|member| collect(member, found));
            return;
        }
        _ => return,
    };
    if name.is_some() {
        found.push(cwl.clone());
    }
}

/// The name of a named type, if it has one.
fn name_of(cwl: &Cwl) -> Option<&str> {
    match cwl {
        Cwl::Record(record) => record.name.as_deref(),
        Cwl::Enum(schema) => schema.name.as_deref(),
        Cwl::Array(array) => array.name.as_deref(),
        _ => None,
    }
}

/// The type that a named type stands for, else the type itself.
pub fn resolve<'c>(cwl: &'c Cwl, named: &'c [Cwl]) -> &'c Cwl {
    let mut current = cwl;
    for _ in 0..DEPTH {
        match current {
            Cwl::Named(name) => match find(name, named) {
                Some(found) => current = found,
                None => break,
            },
            _ => break,
        }
    }
    current
}

/// The named type of a name, by its whole name or else its short name.
fn find<'n>(name: &str, named: &'n [Cwl]) -> Option<&'n Cwl> {
    let exact = named.iter().find(|item| name_of(item) == Some(name));
    let short = |item: &&Cwl| name_of(item).map(short_name) == Some(short_name(name));
    exact.or_else(|| named.iter().find(short))
}

/// The wir type of a CWL type, or why it has none. A union with `null` becomes optional, and
/// a named type is the type of that name in `named`.
pub fn convert(cwl: &Cwl, named: &[Cwl]) -> Result<Type, String> {
    nested(cwl, named, 0)
}

fn nested(cwl: &Cwl, named: &[Cwl], depth: usize) -> Result<Type, String> {
    let convert = |cwl: &Cwl| nested(cwl, named, depth);
    Ok(match cwl {
        Cwl::Boolean => Type::Bool,
        Cwl::Int | Cwl::Long => Type::Int,
        Cwl::Float | Cwl::Double => Type::Float,
        Cwl::String => Type::String,
        Cwl::File | Cwl::Stdout | Cwl::Stderr => Type::File(FileType::default()),
        Cwl::Directory => Type::Directory,
        Cwl::Any => Type::Dynamic,
        Cwl::Array(array) => Type::Array(ArrayType {
            items: Box::new(convert(&array.items)?),
            nonempty: false,
        }),
        Cwl::Record(record) => Type::Record(
            (record.fields.iter())
                .map(|field| {
                    Ok(Field {
                        name: short_name(&field.name).to_owned(),
                        r#type: convert(&field.value_type)?,
                    })
                })
                .collect::<Result<_, String>>()?,
        ),
        Cwl::Enum(schema) => Type::Enum(
            (schema.symbols.iter())
                .map(|symbol| short_name(symbol).to_owned())
                .collect(),
        ),
        Cwl::Union(members) => union(members, named, depth)?,
        Cwl::Null => return Err("a parameter of type null".into()),
        Cwl::Stdin => return Err("a stdin input".into()),
        Cwl::Named(name) => match find(name, named) {
            Some(found) if depth < DEPTH => nested(found, named, depth + 1)?,
            _ => return Err(format!("the named type {}", short_name(name))),
        },
    })
}

/// A type that also holds null.
pub fn optional(r#type: Type) -> Type {
    match r#type {
        Type::Optional(_) => r#type,
        other => Type::Optional(Box::new(other)),
    }
}

fn union(members: &[Cwl], named: &[Cwl], depth: usize) -> Result<Type, String> {
    let mut types: Vec<Type> = (members.iter())
        .filter(|member| **member != Cwl::Null)
        .map(|member| nested(member, named, depth))
        .collect::<Result<_, _>>()?;
    let inner = match types.len() {
        0 => return Err("a parameter of type null".into()),
        1 => types.remove(0),
        _ => Type::Union(types),
    };
    match members.contains(&Cwl::Null) {
        true => Ok(optional(inner)),
        false => Ok(inner),
    }
}
