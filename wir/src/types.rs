//! The data types of wir ports, and when a port of one type accepts a value of another.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde::{Deserialize, Serialize};

/// A data type. `int` is a signed 64-bit integer and `float` an IEEE 754 double; an `int`
/// widens to a `float`, and nothing narrows implicitly. The README states all type rules.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Type {
    Bool,
    Int,
    Float,
    String,
    /// A file; its value keeps location, basename, format, secondary files and contents.
    File(FileType),
    /// A directory; its value keeps location, basename and an optional listing.
    Directory,
    /// Named fields in order. A missing field is allowed only where its type is optional.
    Record(Vec<Field>),
    Array(ArrayType),
    Map(MapType),
    /// The value or null. A bound null is a value, so it differs from a closed empty stream.
    Optional(Box<Type>),
    /// A value of any one member type.
    Union(Vec<Type>),
    /// One of the listed symbols; the value is a string.
    Enum(Vec<String>),
    /// A fixed number of items, each with its own type. A pair is a tuple of two.
    Tuple(Vec<Type>),
    /// A type defined by name in the document's `types`.
    Named(String),
    /// Any value. The reading port's type is checked at run time, with [`crate::fits`].
    Dynamic,
}

/// What a file type requires of its values.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileType {
    /// The format as an IRI, an EDAM IRI where one is known. None accepts any format.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secondary_files: Vec<SecondaryFile>,
}

/// A file that travels with the primary file, such as an index.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecondaryFile {
    /// A suffix added to the primary basename; each leading `^` first removes one extension.
    pub pattern: String,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub required: bool,
}

/// One field of a record.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Field {
    pub name: String,
    pub r#type: Type,
}

/// An array of items of one type.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArrayType {
    pub items: Box<Type>,
    /// Whether the array has at least one item, like a WDL `Array[T]+`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub nonempty: bool,
}

/// A map from keys to values of one type each.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MapType {
    pub key: KeyType,
    pub value: Box<Type>,
}

/// The types a map key may have. In JSON every key is a string: an `int` key is its decimal
/// text and a `bool` key is `true` or `false`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyType {
    String,
    Int,
    Bool,
}

/// A named type definition of a document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypeDef {
    pub name: String,
    pub r#type: Type,
}

impl Type {
    /// Whether a port of this type accepts a value of type `source`. Definitions must not be
    /// recursive through records, arrays or other types; [`crate::verify`] checks that.
    pub fn accepts(&self, source: &Type, defs: &[TypeDef]) -> bool {
        let (Some(target), Some(source)) = (resolve(self, defs), resolve(source, defs)) else {
            return false;
        };
        match (target, source) {
            (Type::Dynamic, _) | (_, Type::Dynamic) => true,
            (Type::Optional(want), Type::Optional(found)) => want.accepts(found, defs),
            (Type::Optional(want), found) => want.accepts(found, defs),
            (_, Type::Union(members)) => members.iter().all(|item| target.accepts(item, defs)),
            (Type::Union(members), found) => members.iter().any(|item| item.accepts(found, defs)),
            (Type::Float, Type::Int) | (Type::String, Type::Enum(_)) => true,
            (Type::Enum(want), Type::Enum(found)) => found.iter().all(|item| want.contains(item)),
            (Type::File(want), Type::File(found)) => want.accepts(found),
            (Type::Record(want), Type::Record(found)) => records_fit(want, found, defs),
            (Type::Array(want), Type::Array(found)) => {
                (!want.nonempty || found.nonempty) && want.items.accepts(&found.items, defs)
            }
            (Type::Map(want), Type::Map(found)) => {
                want.key == found.key && want.value.accepts(&found.value, defs)
            }
            (Type::Tuple(want), Type::Tuple(found)) => {
                want.len() == found.len() && want.iter().zip(found).all(|(a, b)| a.accepts(b, defs))
            }
            (want, found) => want == found,
        }
    }

    /// Whether a null value or an unconnected input fits this type.
    pub fn is_optional(&self, defs: &[TypeDef]) -> bool {
        match resolve(self, defs) {
            Some(Type::Optional(_) | Type::Dynamic) => true,
            Some(Type::Union(members)) => members.iter().any(|item| item.is_optional(defs)),
            _ => false,
        }
    }
}

impl Type {
    /// Removes the formats of the File types in it.
    pub fn unformat(&mut self) {
        match self {
            Type::File(file) => file.format = None,
            Type::Optional(inner) => inner.unformat(),
            Type::Array(array) => array.items.unformat(),
            Type::Map(map) => map.value.unformat(),
            Type::Record(fields) => fields.iter_mut().for_each(|field| field.r#type.unformat()),
            Type::Union(items) | Type::Tuple(items) => items.iter_mut().for_each(Type::unformat),
            _ => {}
        }
    }
}

impl FileType {
    /// The format must match exactly when one is wanted, unless the document has ontologies,
    /// and every required secondary file must be required by the source too.
    pub fn accepts(&self, source: &FileType) -> bool {
        let format = self.format.is_none() || self.format == source.format;
        format
            && self
                .secondary_files
                .iter()
                .filter(|item| item.required)
                .all(|item| {
                    (source.secondary_files.iter())
                        .any(|found| found.required && found.pattern == item.pattern)
                })
    }
}

/// Follows named types to a structural type. None when a name is unknown or the names form a
/// cycle: a chain longer than the number of definitions must repeat one.
pub fn resolve<'a>(r#type: &'a Type, defs: &'a [TypeDef]) -> Option<&'a Type> {
    let mut current = r#type;
    for _ in 0..=defs.len() {
        let Type::Named(name) = current else {
            return Some(current);
        };
        current = &defs.iter().find(|item| item.name == *name)?.r#type;
    }
    None
}

fn records_fit(want: &[Field], found: &[Field], defs: &[TypeDef]) -> bool {
    let known = found
        .iter()
        .all(|item| want.iter().any(|w| w.name == item.name));
    known
        && want
            .iter()
            .all(|w| match found.iter().find(|f| f.name == w.name) {
                Some(field) => w.r#type.accepts(&field.r#type, defs),
                None => w.r#type.is_optional(defs),
            })
}

fn yes() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn parse(value: serde_json::Value) -> Type {
        serde_json::from_value(value).expect("a valid type")
    }

    #[test]
    fn numbers_widen_only() {
        assert!(Type::Float.accepts(&Type::Int, &[]));
        assert!(!Type::Int.accepts(&Type::Float, &[]));
        assert!(!Type::String.accepts(&Type::Int, &[]));
    }

    #[test]
    fn optional_needs_select() {
        let optional = parse(json!({"optional": "int"}));
        assert!(optional.accepts(&Type::Int, &[]));
        assert!(!Type::Int.accepts(&optional, &[]));
        assert!(optional.is_optional(&[]));
    }

    #[test]
    fn unions_and_enums() {
        let union = parse(json!({"union": ["int", "string"]}));
        assert!(union.accepts(&Type::Int, &[]));
        assert!(!Type::Int.accepts(&union, &[]));
        assert!(parse(json!({"union": ["float", "string"]})).accepts(&union, &[]));
        let small = parse(json!({"enum": ["a"]}));
        let large = parse(json!({"enum": ["a", "b"]}));
        assert!(large.accepts(&small, &[]));
        assert!(!small.accepts(&large, &[]));
        assert!(Type::String.accepts(&small, &[]));
    }

    #[test]
    fn nonempty_arrays_narrow() {
        let any = parse(json!({"array": {"items": "int"}}));
        let nonempty = parse(json!({"array": {"items": "int", "nonempty": true}}));
        assert!(any.accepts(&nonempty, &[]));
        assert!(!nonempty.accepts(&any, &[]));
    }

    #[test]
    fn files_check_format() {
        let bam = parse(json!({"file": {"format": "http://edamontology.org/format_2572"}}));
        let indexed = parse(json!({"file": {
            "format": "http://edamontology.org/format_2572",
            "secondary_files": [{"pattern": ".bai"}]
        }}));
        let plain = parse(json!({"file": {}}));
        assert!(plain.accepts(&bam, &[]));
        assert!(!bam.accepts(&plain, &[]));
        assert!(bam.accepts(&indexed, &[]));
        assert!(!indexed.accepts(&bam, &[]));
    }

    #[test]
    fn records_and_names() {
        let defs: Vec<TypeDef> = serde_json::from_value(json!([{"name": "Sample", "type": {
            "record": [{"name": "id", "type": "string"}, {"name": "note", "type": {"optional": "string"}}]
        }}]))
        .expect("valid definitions");
        let sample = Type::Named("Sample".into());
        let short = parse(json!({"record": [{"name": "id", "type": "string"}]}));
        let extra = parse(json!({"record": [
            {"name": "id", "type": "string"}, {"name": "size", "type": "int"}
        ]}));
        assert!(sample.accepts(&short, &defs));
        assert!(!sample.accepts(&extra, &defs));
        assert!(!short.accepts(&sample, &defs));
        assert!(!Type::Named("Missing".into()).accepts(&Type::Int, &defs));
    }

    #[test]
    fn name_cycles_fail() {
        let defs = vec![
            TypeDef {
                name: "A".into(),
                r#type: Type::Named("B".into()),
            },
            TypeDef {
                name: "B".into(),
                r#type: Type::Named("A".into()),
            },
        ];
        assert_eq!(resolve(&Type::Named("A".into()), &defs), None);
    }
}
