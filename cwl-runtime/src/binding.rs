//! Command line bindings: how input values, `baseCommand` and `arguments` become sorted
//! bindings, and how each binding becomes arguments. This follows cwltool's `bind_input`,
//! `generate_arg` and its sort of positions and names.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cmp::Ordering;

use cwl_parser::model::{CommandLineBinding, Dynamic, Expression, Type};

use crate::types::Types;
use crate::value::{Value, plain_float};

/// One part of the sort key of a binding: a position, or the name of a record field.
#[derive(Clone, Debug, PartialEq)]
pub enum Key {
    Number(i64),
    Name(String),
}

impl Key {
    fn text(&self) -> String {
        match self {
            Key::Number(number) => number.to_string(),
            Key::Name(name) => name.clone(),
        }
    }
}

/// A binding with the value it binds, before its `valueFrom` runs.
#[derive(Clone, Debug, PartialEq)]
pub struct Bound {
    pub key: Vec<Key>,
    pub value: Value,
    pub prefix: Option<String>,
    pub separate: bool,
    pub item_separator: Option<String>,
    pub value_from: Option<Expression>,
    pub shell_quote: bool,
}

impl Bound {
    /// A binding of a fixed text, like an entry of `baseCommand`.
    pub fn text(key: Vec<Key>, text: &str) -> Self {
        Bound {
            key,
            value: Value::string(text),
            prefix: None,
            separate: true,
            item_separator: None,
            value_from: None,
            shell_quote: true,
        }
    }
}

/// An argument of the command line, and whether it must be quoted for a shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Arg {
    pub text: String,
    pub quote: bool,
}

/// Sorts bindings as cwltool does: by the key lists, element by element, where a shorter
/// list comes first and a name compares as text with anything.
pub fn sort(bindings: &mut [Bound]) {
    bindings.sort_by(|a, b| compare_keys(&a.key, &b.key));
}

fn compare_keys(a: &[Key], b: &[Key]) -> Ordering {
    for index in 0..a.len().max(b.len()) {
        match (a.get(index), b.get(index)) {
            (Some(x), Some(y)) if x == y => continue,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(Key::Number(x)), Some(Key::Number(y))) => return x.cmp(y),
            (Some(x), Some(y)) => {
                return if x.text() > y.text() {
                    Ordering::Greater
                } else {
                    Ordering::Less
                };
            }
        }
    }
    Ordering::Equal
}

/// The settings of a binding on its way down arrays: a fresh one from the document, or the
/// copy of the binding of the enclosing array, whose position is already known.
#[derive(Clone, Debug)]
enum Position {
    Given(Option<Dynamic<i64>>),
    Known(Vec<Key>),
}

#[derive(Clone, Debug)]
struct Spec {
    position: Position,
    prefix: Option<String>,
    separate: bool,
    item_separator: Option<String>,
    value_from: Option<Expression>,
    shell_quote: bool,
}

impl Spec {
    fn from_binding(binding: &CommandLineBinding) -> Self {
        Spec {
            position: Position::Given(binding.position.clone()),
            prefix: binding.prefix.clone(),
            separate: binding.separate,
            item_separator: binding.item_separator.clone(),
            value_from: binding.value_from.clone(),
            shell_quote: binding.shell_quote,
        }
    }

    fn empty() -> Self {
        Spec {
            position: Position::Given(None),
            prefix: None,
            separate: true,
            item_separator: None,
            value_from: None,
            shell_quote: true,
        }
    }

    fn inherited(bound: &Bound) -> Self {
        Spec {
            position: Position::Known(bound.key.clone()),
            prefix: bound.prefix.clone(),
            separate: bound.separate,
            item_separator: bound.item_separator.clone(),
            value_from: bound.value_from.clone(),
            shell_quote: bound.shell_quote,
        }
    }
}

/// Evaluates position expressions, with `self` set to the value being bound.
pub type Positions<'a> = dyn FnMut(&Expression, &Value) -> Result<Value, String> + 'a;

/// Binds input values to the command line.
pub struct Binder<'a, 'b> {
    pub types: &'a Types,
    pub positions: &'a mut Positions<'b>,
}

impl Binder<'_, '_> {
    /// The bindings of one input parameter, whose name ends the sort keys.
    pub fn bind_input(
        &mut self,
        value_type: &Type,
        binding: Option<&CommandLineBinding>,
        value: &Value,
        name: &str,
    ) -> Result<Vec<Bound>, String> {
        let spec = binding.map(Spec::from_binding);
        self.bind(value_type, spec, value, &[], &[Key::Name(name.to_owned())])
    }

    /// cwltool's `bind_input` for a schema with this type and binding.
    fn bind(
        &mut self,
        value_type: &Type,
        spec: Option<Spec>,
        datum: &Value,
        lead: &[Key],
        tail: &[Key],
    ) -> Result<Vec<Bound>, String> {
        let own = self.own(spec.clone(), datum, lead, tail)?;
        let value_from = own.as_ref().is_some_and(|own| own.value_from.is_some());
        let mut bindings = Vec::new();
        match value_type {
            Type::Union(members) => {
                let mut bound = false;
                for member in members {
                    if !self.types.matches(member, datum) {
                        continue;
                    }
                    let inner = self.bind(member, spec.clone(), datum, lead, tail)?;
                    if !value_from {
                        return Ok(inner);
                    }
                    bound = true;
                }
                if !bound {
                    return Err(format!(
                        "{} does not fit any type of the union",
                        datum.to_json()
                    ));
                }
            }
            Type::Array(_) | Type::Record(_) | Type::Enum(_) => {
                let (schema_binding, is_array) = match value_type {
                    Type::Array(array) => (array.binding.as_ref(), true),
                    Type::Record(record) => (record.binding.as_ref(), false),
                    Type::Enum(schema) => (schema.binding.as_ref(), false),
                    _ => (None, false),
                };
                let mut inner_spec = schema_binding.map(Spec::from_binding);
                let separated = own.as_ref().is_some_and(|own| own.item_separator.is_some());
                if own.is_some() && inner_spec.is_none() && is_array && !separated {
                    inner_spec = Some(Spec::empty());
                }
                let mut inner_own = self.own(inner_spec, datum, lead, tail)?;
                let inner = self.leaf(value_type, &mut inner_own, datum, lead, tail)?;
                let inner = finish(inner_own, inner);
                if !value_from {
                    bindings.extend(inner);
                }
            }
            _ => {
                let mut own = own;
                let inner = self.leaf(value_type, &mut own, datum, lead, tail)?;
                return Ok(finish(own, inner));
            }
        }
        Ok(finish(own, bindings))
    }

    /// The binding a spec gives: its position after `lead` and before `tail`.
    fn own(
        &mut self,
        spec: Option<Spec>,
        datum: &Value,
        lead: &[Key],
        tail: &[Key],
    ) -> Result<Option<Bound>, String> {
        let Some(spec) = spec else {
            return Ok(None);
        };
        let mut key = lead.to_vec();
        match spec.position {
            Position::Given(None) => key.push(Key::Number(0)),
            Position::Given(Some(Dynamic::Value(position))) => key.push(Key::Number(position)),
            Position::Given(Some(Dynamic::Expression(expression))) => {
                let value = (self.positions)(&expression, datum)?;
                let position = value.as_i64().ok_or_else(|| {
                    format!("`position` must be an integer, not {}", value.to_json())
                })?;
                key.push(Key::Number(position));
            }
            Position::Known(known) => key.extend(known),
        }
        key.extend(tail.iter().cloned());
        Ok(Some(Bound {
            key,
            value: datum.clone(),
            prefix: spec.prefix,
            separate: spec.separate,
            item_separator: spec.item_separator,
            value_from: spec.value_from,
            shell_quote: spec.shell_quote,
        }))
    }

    /// The part of `bind_input` for a named or primitive type, or for an inline schema
    /// reached from its enclosing parameter: records bind their fields, arrays their items.
    fn leaf(
        &mut self,
        value_type: &Type,
        own: &mut Option<Bound>,
        datum: &Value,
        lead: &[Key],
        tail: &[Key],
    ) -> Result<Vec<Bound>, String> {
        let mut bindings = Vec::new();
        let resolved = self.types.resolve(value_type).clone();
        let resolved = match resolved {
            Type::Any => any_type(datum),
            other => other,
        };
        match &resolved {
            Type::Record(record) => {
                let Value::Map(map) = datum else {
                    return Ok(bindings);
                };
                for field in &record.fields {
                    let Some(value) = map.get(&field.name).filter(|value| !value.is_null()) else {
                        continue;
                    };
                    let spec = field.input_binding.as_ref().map(Spec::from_binding);
                    let name = [Key::Name(field.name.clone())];
                    bindings.extend(self.bind(&field.value_type, spec, value, lead, &name)?);
                }
            }
            Type::Array(array) => {
                let Value::List(items) = datum else {
                    return Ok(bindings);
                };
                for (index, item) in items.iter().enumerate() {
                    let spec = own.as_ref().map(Spec::inherited);
                    let lead = [Key::Number(index as i64)];
                    bindings.extend(self.bind(&array.items, spec, item, &lead, tail)?);
                }
                *own = None;
            }
            _ => {}
        }
        Ok(bindings)
    }
}
