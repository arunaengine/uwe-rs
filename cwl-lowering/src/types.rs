//! The wir types of CWL parameter types.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use cwl_parser::model::{Type as Cwl, short_name};
use wir::{ArrayType, Field, FileType, Type};

/// The wir type of a CWL type, or why it has none. A union with `null` becomes optional.
pub fn convert(cwl: &Cwl) -> Result<Type, String> {
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
        Cwl::Union(members) => union(members)?,
        Cwl::Null => return Err("a parameter of type null".into()),
        Cwl::Stdin => return Err("a stdin input".into()),
        Cwl::Named(name) => return Err(format!("the named type {}", short_name(name))),
    })
}

/// A type that also holds null.
pub fn optional(r#type: Type) -> Type {
    match r#type {
        Type::Optional(_) => r#type,
        other => Type::Optional(Box::new(other)),
    }
}

fn union(members: &[Cwl]) -> Result<Type, String> {
    let mut types: Vec<Type> = (members.iter())
        .filter(|member| **member != Cwl::Null)
        .map(convert)
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
