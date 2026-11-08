//! The wir types of WDL types, and the wir ports of the inputs and outputs of a WDL callable.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use wdl_analysis::types::{CompoundType, CustomType, Optional, PrimitiveType, Type as Wdl};
use wir::{ArrayType, Field, FileType, Flow, KeyType, MapType, Port, Type};

/// The wir type of a WDL type: a pair is a tuple of two, a struct a record, an `Object`
/// dynamic.
pub(crate) fn convert(ty: &Wdl) -> Result<Type, String> {
    let inner = match ty {
        Wdl::Primitive(primitive, _) => match primitive {
            PrimitiveType::Boolean => Type::Bool,
            PrimitiveType::Integer => Type::Int,
            PrimitiveType::Float => Type::Float,
            PrimitiveType::String => Type::String,
            PrimitiveType::File => Type::File(FileType::default()),
            PrimitiveType::Directory => Type::Directory,
        },
        Wdl::Compound(CompoundType::Array(array), _) => Type::Array(ArrayType {
            items: Box::new(convert(array.element_type())?),
            nonempty: array.is_non_empty(),
        }),
        Wdl::Compound(CompoundType::Pair(pair), _) => Type::Tuple(vec![
            convert(pair.left_type())?,
            convert(pair.right_type())?,
        ]),
        Wdl::Compound(CompoundType::Map(map), _) => {
            let key = match map.key_type().as_primitive() {
                Some(PrimitiveType::Integer) => KeyType::Int,
                Some(PrimitiveType::Boolean) => KeyType::Bool,
                Some(PrimitiveType::String | PrimitiveType::File | PrimitiveType::Directory) => {
                    KeyType::String
                }
                _ => return Err(format!("the map key type of {ty}")),
            };
            let value = Box::new(convert(map.value_type())?);
            Type::Map(MapType { key, value })
        }
        Wdl::Compound(CompoundType::Custom(CustomType::Struct(record)), _) => Type::Record(
            (record.members().iter())
                .map(|(name, member)| {
                    let r#type = convert(member)?;
                    Ok(Field {
                        name: name.clone(),
                        r#type,
                    })
                })
                .collect::<Result<_, String>>()?,
        ),
        Wdl::Object | Wdl::OptionalObject | Wdl::Union | Wdl::None => Type::Dynamic,
        _ => return Err(format!("the type {ty}")),
    };
    Ok(match ty.is_optional() && inner != Type::Dynamic {
        true => optional(inner),
        false => inner,
    })
}

pub(crate) fn optional(inner: Type) -> Type {
    match inner {
        Type::Optional(_) | Type::Dynamic => inner,
        other => Type::Optional(Box::new(other)),
    }
}

pub(crate) fn port(name: &str, r#type: Type) -> Port {
    Port {
        name: name.to_owned(),
        r#type,
        flow: Flow::Value,
        default: None,
    }
}
