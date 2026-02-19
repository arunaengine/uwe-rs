//! Decodes typed values from value trees with serde, keeping paths, positions and unknown fields.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::fmt;

use serde::de::value::StrDeserializer;
use serde::de::{
    self, DeserializeOwned, DeserializeSeed, Deserializer, IntoDeserializer, MapAccess, SeqAccess,
    Unexpected, Visitor,
};

use crate::node::{Entry, Node, Position, Value};

/// A decoding error with the path and position of the value it is about.
#[derive(Clone, Debug, PartialEq)]
pub struct DecodeError {
    pub message: String,
    pub path: String,
    pub position: Option<Position>,
}

/// A mapping key that the typed model has no field for.
#[derive(Clone, Debug, PartialEq)]
pub struct UnknownField {
    pub path: String,
    pub position: Position,
}

/// Decodes `T` from `node` and lists the unknown fields it met.
pub fn decode<T: DeserializeOwned>(node: &Node) -> Result<(T, Vec<UnknownField>), DecodeError> {
    let unknown = RefCell::new(Vec::new());
    let reader = Reader {
        node,
        path: &Path::Root,
        unknown: &unknown,
        key: None,
    };
    let value = T::deserialize(reader).map_err(|error| error.locate(node, &Path::Root))?;
    Ok((value, unknown.into_inner()))
}

/// The struct name that asks the reader for the node itself, for fields kept as raw values.
pub(crate) const RAW_NAME: &str = "$argo_parser::Node";

thread_local! {
    /// Hands a raw node from the reader to the visitor of `Node`, which serde cannot carry.
    static RAW: RefCell<Option<Node>> = const { RefCell::new(None) };
}

impl<'de> de::Deserialize<'de> for Node {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_newtype_struct(RAW_NAME, RawVisitor)
    }
}

struct RawVisitor;

impl<'de> Visitor<'de> for RawVisitor {
    type Value = Node;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a value read by argo-parser")
    }

    fn visit_unit<E: de::Error>(self) -> Result<Node, E> {
        RAW.with(|slot| slot.borrow_mut().take())
            .ok_or_else(|| E::custom("raw values can only be decoded by argo-parser"))
    }
}

/// The path of a value from the document root, like `spec.templates[0].name`.
enum Path<'a> {
    Root,
    Field(&'a Path<'a>, &'a str),
    Index(&'a Path<'a>, usize),
}

impl fmt::Display for Path<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Path::Root => Ok(()),
            Path::Field(Path::Root, name) => f.write_str(name),
            Path::Field(parent, name) => write!(f, "{parent}.{name}"),
            Path::Index(parent, index) => write!(f, "{parent}[{index}]"),
        }
    }
}

impl DecodeError {
    /// Sets the path and position once, so the innermost value that failed wins.
    fn locate(mut self, node: &Node, path: &Path<'_>) -> Self {
        if self.position.is_none() {
            self.position = Some(node.position);
            self.path = path.to_string();
        }
        self
    }
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.path.is_empty() {
            f.write_str(&self.message)
        } else {
            write!(f, "{}: {}", self.path, self.message)
        }
    }
}

impl std::error::Error for DecodeError {}

impl de::Error for DecodeError {
    fn custom<T: fmt::Display>(message: T) -> Self {
        Self {
            message: message.to_string(),
            path: String::new(),
            position: None,
        }
    }
}

/// A deserializer for one node. `key` is the key position of a struct field value, where
/// serde's request to ignore the value means the key is unknown.
#[derive(Clone, Copy)]
struct Reader<'a> {
    node: &'a Node,
    path: &'a Path<'a>,
    unknown: &'a RefCell<Vec<UnknownField>>,
    key: Option<Position>,
}

impl<'a> Reader<'a> {
    fn child<'b>(&self, node: &'b Node, path: &'b Path<'b>, key: Option<Position>) -> Reader<'b>
    where
        'a: 'b,
    {
        Reader {
            node,
            path,
            unknown: self.unknown,
            key,
        }
    }

    fn fail<V>(self, visitor: &dyn de::Expected) -> Result<V, DecodeError> {
        Err(de::Error::invalid_type(unexpected(self.node), visitor))
    }

    fn locate<V>(self, result: Result<V, DecodeError>) -> Result<V, DecodeError> {
        result.map_err(|error| error.locate(self.node, self.path))
    }
}

fn unexpected(node: &Node) -> Unexpected<'_> {
    match &node.value {
        Value::Null => Unexpected::Unit,
        Value::Bool(flag) => Unexpected::Bool(*flag),
        Value::Integer(number) => Unexpected::Signed(*number),
        Value::Unsigned(number) => Unexpected::Unsigned(*number),
        Value::Float(number) => Unexpected::Float(*number),
        Value::String(text) => Unexpected::Str(text),
        Value::Sequence(_) => Unexpected::Seq,
        Value::Mapping(_) => Unexpected::Map,
    }
}

/// Go's JSON decoding leaves a field at its zero value when it is `null`; the readers below
/// do the same, so `outputs: null` or `name: null` load like absent fields.
impl<'de, 'a> Deserializer<'de> for Reader<'a> {
    type Error = DecodeError;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DecodeError> {
        let result = match &self.node.value {
            Value::Null => visitor.visit_unit(),
            Value::Bool(flag) => visitor.visit_bool(*flag),
            Value::Integer(number) => visitor.visit_i64(*number),
            Value::Unsigned(number) => visitor.visit_u64(*number),
            Value::Float(number) => visitor.visit_f64(*number),
            Value::String(text) => visitor.visit_str(text),
            Value::Sequence(items) => visitor.visit_seq(Items::new(self, items)),
            Value::Mapping(entries) => visitor.visit_map(Fields::new(self, entries, false)),
        };
        self.locate(result)
    }

    fn deserialize_bool<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DecodeError> {
        let result = match self.node.value {
            Value::Null => visitor.visit_bool(false),
            Value::Bool(flag) => visitor.visit_bool(flag),
            _ => self.fail(&visitor),
        };
        self.locate(result)
    }

    fn deserialize_i64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DecodeError> {
        let result = match self.node.value {
            Value::Null => visitor.visit_i64(0),
            Value::Integer(number) => visitor.visit_i64(number),
            Value::Unsigned(number) => visitor.visit_u64(number),
            // YAML `2.0` reaches Argo's JSON decoder as `2`, which fits an integer field.
            Value::Float(number) if number.fract() == 0.0 && number.abs() < 9.0e15 => {
                visitor.visit_i64(number as i64)
            }
            _ => self.fail(&visitor),
        };
        self.locate(result)
    }

    fn deserialize_f64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DecodeError> {
        let result = match self.node.value {
            Value::Null => visitor.visit_f64(0.0),
            Value::Integer(number) => visitor.visit_f64(number as f64),
            Value::Unsigned(number) => visitor.visit_f64(number as f64),
            Value::Float(number) => visitor.visit_f64(number),
            _ => self.fail(&visitor),
        };
        self.locate(result)
    }

    fn deserialize_str<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DecodeError> {
        let result = match &self.node.value {
            Value::Null => visitor.visit_str(""),
            Value::String(text) => visitor.visit_str(text),
            _ => self.fail(&visitor),
        };
        self.locate(result)
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DecodeError> {
        let result = match self.node.value {
            Value::Null => visitor.visit_none(),
            _ => visitor.visit_some(self),
        };
        self.locate(result)
    }

    fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DecodeError> {
        let result = match &self.node.value {
            Value::Null => visitor.visit_seq(Items::new(self, &[])),
            Value::Sequence(items) => visitor.visit_seq(Items::new(self, items)),
            _ => self.fail(&visitor),
        };
        self.locate(result)
    }

    fn deserialize_map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DecodeError> {
        self.read_mapping(visitor, false)
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, DecodeError> {
        self.read_mapping(visitor, true)
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, DecodeError> {
        if name == RAW_NAME {
            RAW.with(|slot| *slot.borrow_mut() = Some(self.node.clone()));
            return self.locate(visitor.visit_unit());
        }
        self.locate(visitor.visit_newtype_struct(self))
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, DecodeError> {
        let result = match &self.node.value {
            Value::String(text) => {
                let variant: StrDeserializer<'_, DecodeError> = text.as_str().into_deserializer();
                visitor.visit_enum(variant)
            }
            _ => self.fail(&visitor),
        };
        self.locate(result)
    }

    fn deserialize_ignored_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DecodeError> {
        if let Some(position) = self.key {
            self.unknown.borrow_mut().push(UnknownField {
                path: self.path.to_string(),
                position,
            });
        }
        visitor.visit_unit()
    }

    fn deserialize_i8<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DecodeError> {
        self.deserialize_i64(visitor)
    }

    fn deserialize_i16<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DecodeError> {
        self.deserialize_i64(visitor)
    }

    fn deserialize_i32<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DecodeError> {
        self.deserialize_i64(visitor)
    }

    fn deserialize_u8<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DecodeError> {
        self.deserialize_i64(visitor)
    }

    fn deserialize_u16<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DecodeError> {
        self.deserialize_i64(visitor)
    }

    fn deserialize_u32<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DecodeError> {
        self.deserialize_i64(visitor)
    }

    fn deserialize_u64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DecodeError> {
        self.deserialize_i64(visitor)
    }

    fn deserialize_f32<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DecodeError> {
        self.deserialize_f64(visitor)
    }

    fn deserialize_string<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DecodeError> {
        self.deserialize_str(visitor)
    }

    serde::forward_to_deserialize_any! {
        i128 u128 char bytes byte_buf unit unit_struct tuple tuple_struct identifier
    }
}

impl<'de> Reader<'_> {
    fn read_mapping<V: Visitor<'de>>(
        self,
        visitor: V,
        typed: bool,
    ) -> Result<V::Value, DecodeError> {
        let result = match &self.node.value {
            Value::Null => visitor.visit_map(Fields::new(self, &[], typed)),
            Value::Mapping(entries) => visitor.visit_map(Fields::new(self, entries, typed)),
            _ => self.fail(&visitor),
        };
        self.locate(result)
    }
}

struct Items<'a> {
    reader: Reader<'a>,
    items: std::slice::Iter<'a, Node>,
    index: usize,
}

impl<'a> Items<'a> {
    fn new(reader: Reader<'a>, items: &'a [Node]) -> Self {
        Self {
            reader,
            items: items.iter(),
            index: 0,
        }
    }
}

impl<'de> SeqAccess<'de> for Items<'_> {
    type Error = DecodeError;

    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, DecodeError> {
        let Some(node) = self.items.next() else {
            return Ok(None);
        };
        let path = Path::Index(self.reader.path, self.index);
        self.index += 1;
        let reader = self.reader.child(node, &path, None);
        reader.locate(seed.deserialize(reader)).map(Some)
    }
}

/// Mapping entries. For typed structs, `typed` makes ignored keys count as unknown fields.
struct Fields<'a> {
    reader: Reader<'a>,
    entries: std::slice::Iter<'a, Entry>,
    current: Option<&'a Entry>,
    typed: bool,
}

impl<'a> Fields<'a> {
    fn new(reader: Reader<'a>, entries: &'a [Entry], typed: bool) -> Self {
        Self {
            reader,
            entries: entries.iter(),
            current: None,
            typed,
        }
    }
}

impl<'de> MapAccess<'de> for Fields<'_> {
    type Error = DecodeError;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, DecodeError> {
        let Some(entry) = self.entries.next() else {
            return Ok(None);
        };
        self.current = Some(entry);
        let key: StrDeserializer<'_, DecodeError> = entry.key.as_str().into_deserializer();
        seed.deserialize(key).map(Some)
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(
        &mut self,
        seed: V,
    ) -> Result<V::Value, DecodeError> {
        let entry = self
            .current
            .take()
            .ok_or_else(|| <DecodeError as de::Error>::custom("value without a key"))?;
        let path = Path::Field(self.reader.path, &entry.key);
        let key = self.typed.then_some(entry.position);
        let reader = self.reader.child(&entry.value, &path, key);
        reader.locate(seed.deserialize(reader))
    }
}

#[cfg(test)]
#[path = "decode_tests.rs"]
mod tests;
