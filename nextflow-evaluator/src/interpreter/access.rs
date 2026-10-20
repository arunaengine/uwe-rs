//! Properties, indexes, method calls, casts, `instanceof` and `new`, for the classes whose
//! rules are part of the core. The standard library adds the other methods.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_parser::Span;

use super::library::org_json;
use super::{AtSpan, Error, Interpreter, thrown, unsupported};
use crate::exception::{self, Exception};
use crate::number::Number;
use crate::value::{Key, List, ListKind, MapKind, Object, PathValue, State, Value};

impl Interpreter<'_> {
    /// `object.name`. A map reads the key, so `m.class` is `m.get('class')`.
    pub(super) fn property(
        &mut self,
        object: &Value,
        name: &str,
        span: Span,
    ) -> Result<Value, Error> {
        Ok(match object {
            Value::Namespace(namespace) if &**namespace == "secrets" => {
                return match self.host.secret(name)? {
                    None => Ok(Value::Null),
                    Some(_) => Err(unsupported(
                        "secret values in workflow expressions; use a process `secret` directive",
                        span,
                    )),
                };
            }
            Value::Null => {
                let message = format!("Cannot get property '{name}' on null object");
                return Err(thrown(
                    Exception::new("NullPointerException", message),
                    span,
                ));
            }
            Value::Map(map) => {
                let key = Key(Value::string(name));
                let value = map.borrow().entries.get(&key).cloned();
                match value {
                    Some(Value::Closure(closure)) if map.borrow().kind == MapKind::Lazy => {
                        self.call_closure_value(&closure, Vec::new(), span)?
                    }
                    Some(value) => value,
                    None => self.default_value(map, &key.0, span)?,
                }
            }
            Value::ChannelOut(out) => {
                let found = out
                    .names
                    .iter()
                    .position(|emit| emit.as_deref() == Some(name));
                match found {
                    Some(index) => out.values[index].clone(),
                    // Nextflow reads the property of each output channel, which gives a list
                    // of expressions without a value.
                    None => Value::list(vec![Value::Null; out.values.len()]),
                }
            }
            Value::Definition(definition) => self.definition_property(definition, name, span)?,
            Value::Channel(_) => match self.host.channel_property(object, name) {
                Ok(Some(value)) => value,
                Ok(None) => return self.getter(object, name, span),
                Err(error) => return Err(error.or_at(span)),
            },
            Value::Object(object) if super::classes::is_library(object) => {
                self.library_property(object, name, span)?
            }
            Value::Object(object) if matches!(object.borrow().state, State::GroupKey(..)) => {
                let State::GroupKey(target, size) = &object.borrow().state else {
                    unreachable!("a group key")
                };
                match name {
                    "groupSize" => Value::integer(*size),
                    "groupTarget" => target.clone(),
                    _ => self.property(&target.clone(), name, span)?,
                }
            }
            // Groovy reads a property of a `JSONObject` with its `get(name)`.
            Value::Object(json) if org_json::is_json_object(json) => {
                self.call_method(object, "get", vec![Value::string(name)], span)?
            }
            Value::Object(object) if matches!(object.borrow().state, State::Workflow(_)) => {
                let properties = match &object.borrow().state {
                    State::Workflow(workflow) => workflow.properties.clone(),
                    _ => unreachable!("a workflow"),
                };
                match &properties {
                    Value::Map(map)
                        if map.borrow().entries.contains_key(&Key(Value::string(name))) =>
                    {
                        self.property(&properties, name, span)?
                    }
                    _ => {
                        let message = format!(
                            "No such property: {name} for class: nextflow.script.WorkflowMetadata"
                        );
                        return Err(thrown(
                            Exception::new("MissingPropertyException", message),
                            span,
                        ));
                    }
                }
            }
            Value::Exception(exception) if name == "cause" => match &exception.cause {
                Some(cause) => Value::Exception(Rc::new((**cause).clone())),
                None => Value::Null,
            },
            Value::Exception(exception) if name == "message" => match exception.message.as_str() {
                "" => Value::Null,
                message => Value::string(message),
            },
            // Groovy reads the private field `value` of a boxed Boolean or primitive number, which
            // is the value itself. `BigInteger` and `BigDecimal` have no such field.
            Value::Boolean(_)
            | Value::Number(
                Number::Integer(_) | Number::Long(_) | Number::Double(_) | Number::Float(_),
            ) if name == "value" => object.clone(),
            // A Java array, like the result of `split`, has a `length`.
            Value::List(list) if name == "length" && list.borrow().kind == ListKind::Array => {
                Value::integer(list.borrow().items.len() as i32)
            }
            Value::List(_) | Value::Set(_) => match name {
                "size" | "first" | "last" => self.call_method(object, name, Vec::new(), span)?,
                "empty" => self.call_method(object, "isEmpty", Vec::new(), span)?,
                // Other names are read from each element that is not null.
                _ => {
                    let mut values = Vec::new();
                    for item in super::library::elements(object) {
                        if !matches!(item, Value::Null) {
                            values.push(self.property(&item, name, span)?);
                        }
                    }
                    Value::list(values)
                }
            },
            _ => return self.getter(object, name, span),
        })
    }

    /// A property of an object that is not a map: its getter `getName()` or `isName()`. A number
    /// without such a getter reads a unit, like `2.GB`.
    fn getter(&mut self, object: &Value, name: &str, span: Span) -> Result<Value, Error> {
        let mut capitalized = name.chars();
        let first = capitalized
            .next()
            .map(|c| c.to_uppercase().collect::<String>());
        let capitalized = format!("{}{}", first.unwrap_or_default(), capitalized.as_str());
        for prefix in ["get", "is"] {
            let getter = format!("{prefix}{capitalized}");
            if let Some(value) = self.library_method(object, &getter, &[], span)? {
                return Ok(value);
            }
        }
        if let Value::Number(number) = object {
            return super::library::unit_property(number, name, span);
        }
        let class = object.class_name();
        Err(unsupported(
            format!("the property `{name}` of {class}"),
            span,
        ))
    }

    /// `object.name = value`. Records reject changes.
    pub(super) fn set_property(
        &mut self,
        object: &Value,
        name: &str,
        value: Value,
        span: Span,
    ) -> Result<(), Error> {
        match object {
            Value::Null => {
                let message = format!("Cannot set property '{name}' on null object");
                Err(thrown(
                    Exception::new("NullPointerException", message),
                    span,
                ))
            }
            Value::Map(map) if map.borrow().kind == MapKind::Record => Err(rejected(span)),
            Value::Object(object) if super::classes::is_library(object) => {
                self.set_library_property(object, name, value, span)
            }
            Value::Map(map) => {
                map.borrow_mut()
                    .entries
                    .insert(Key(Value::string(name)), value);
                Ok(())
            }
            Value::Path(_) if name == "text" => {
                self.call_method(object, "setText", vec![value], span)?;
                Ok(())
            }
            // A method added to a metaclass, like `Map.metaClass.deepMerge = { ... }`.
            Value::Namespace(namespace) if namespace.starts_with("metaClass:") => Ok(()),
            _ => {
                let class = object.class_name();
                Err(unsupported(
                    format!("setting the property `{name}` of {class}"),
                    span,
                ))
            }
        }
    }

    /// `object[index]`, with Groovy's rules: past the end of a list is `null`, a negative index
    /// counts from the end, and a string gives a one-character string.
    pub(super) fn index(
        &mut self,
        object: &Value,
        index: &[Value],
        span: Span,
    ) -> Result<Value, Error> {
        // Groovy's `list[0, 2]` indexes with the list of the indexes.
        let several;
        let index = match index {
            [index] => index,
            _ => {
                several = Value::list(index.to_vec());
                &several
            }
        };
        Ok(match (object, index) {
            (Value::Namespace(namespace), key) if &**namespace == "secrets" => {
                return self.property(object, &key.to_string(), span);
            }
            (Value::Path(path), Value::Number(position)) => {
                let items = super::library::path_items(path);
                let position = position.to_i64();
                let position = if position < 0 {
                    items.len() as i64 + position
                } else {
                    position
                };
                match usize::try_from(position).ok().and_then(|p| items.get(p)) {
                    Some(item) => item.clone(),
                    None => Value::Null,
                }
            }
            (Value::Null, _) => {
                let message = "Cannot invoke method getAt() on null object";
                return Err(thrown(
                    Exception::new("NullPointerException", message),
                    span,
                ));
            }
            (Value::Object(json), key) if org_json::is_json_object(json) => {
                self.call_method(object, "get", vec![key.clone()], span)?
            }
            (Value::Map(map), key) => {
                let value = map.borrow().entries.get(&Key(key.clone())).cloned();
                match value {
                    Some(value) => value,
                    None => self.default_value(map, key, span)?,
                }
            }
            // All matches from the start; a pattern with groups gives a list per match.
            (Value::ChannelOut(out), Value::Number(number)) if integral(number) => {
                match position(number.to_i64(), out.values.len()) {
                    Some(position) if position < out.values.len() => out.values[position].clone(),
                    _ => {
                        let message = format!(
                            "Index {number} out of bounds for length {}",
                            out.values.len()
                        );
                        return Err(thrown(
                            Exception::new("IndexOutOfBoundsException", message),
                            span,
                        ));
                    }
                }
            }
            (Value::Matcher(matcher), Value::Number(number)) if integral(number) => {
                let count = matcher.matches.len();
                let groups = match position(number.to_i64(), count).filter(|p| *p < count) {
                    Some(position) => &matcher.matches[position],
                    None => {
                        let message = format!(
                            "index is out of range {}..{} (index = {number})",
                            -(count as i64),
                            count as i64 - 1
                        );
                        return Err(thrown(
                            Exception::new("IndexOutOfBoundsException", message),
                            span,
                        ));
                    }
                };
                let text =
                    |group: &Option<String>| group.as_deref().map_or(Value::Null, Value::string);
                if matcher.pattern.group_count() == 0 {
                    text(&groups[0])
                } else {
                    Value::list(groups.iter().map(text).collect())
                }
            }
            (Value::List(list), Value::List(indexes)) if indexes.borrow().range.is_some() => {
                let list = list.borrow();
                let (from, to, reverse) = borders(&indexes.borrow(), list.items.len(), span)?;
                let mut items = list.items[from..to].to_vec();
                if reverse {
                    items.reverse();
                }
                Value::list(items)
            }
            // Groovy's `getAt(String)` of a collection: the property of each element that is
            // not null.
            (Value::List(list), Value::String(name) | Value::GString(name)) => {
                let items = list.borrow().items.clone();
                let mut values = Vec::new();
                for item in items.iter().filter(|item| !matches!(item, Value::Null)) {
                    values.push(self.property(item, name, span)?);
                }
                Value::list(values)
            }
            // A list of indexes gives the element at each, with ranges spread.
            (Value::List(_), Value::List(indexes)) => {
                let indexes = indexes.borrow().items.clone();
                let mut items = Vec::new();
                for index in indexes {
                    let range =
                        matches!(&index, Value::List(list) if list.borrow().range.is_some());
                    let found = self.index(object, &[index], span)?;
                    match found {
                        Value::List(found) if range => items.extend(found.borrow().items.clone()),
                        found => items.push(found),
                    }
                }
                Value::list(items)
            }
            (Value::String(text) | Value::GString(text), Value::List(indexes))
                if indexes.borrow().range.is_some() =>
            {
                let units: Vec<u16> = text.encode_utf16().collect();
                let (from, to, reverse) = borders(&indexes.borrow(), units.len(), span)?;
                let part = String::from_utf16_lossy(&units[from..to]);
                Value::string(if reverse {
                    part.chars().rev().collect()
                } else {
                    part
                })
            }
            (Value::List(list), Value::Number(number)) if integral(number) => {
                let list = list.borrow();
                let length = list.items.len();
                match position(number.to_i64(), length) {
                    Some(position) if position < length => list.items[position].clone(),
                    Some(_) if list.kind != ListKind::Array => Value::Null,
                    _ => {
                        return Err(out_of_bounds(
                            "ArrayIndexOutOfBoundsException",
                            number,
                            length,
                            span,
                        ));
                    }
                }
            }
            (Value::String(text) | Value::GString(text), Value::Number(number))
                if integral(number) =>
            {
                let units: Vec<u16> = text.encode_utf16().collect();
                match position(number.to_i64(), units.len()) {
                    Some(position) if position < units.len() => {
                        Value::string(String::from_utf16_lossy(&units[position..=position]))
                    }
                    Some(_) => {
                        return Err(out_of_bounds(
                            "StringIndexOutOfBoundsException",
                            number,
                            units.len(),
                            span,
                        ));
                    }
                    None => {
                        return Err(out_of_bounds(
                            "ArrayIndexOutOfBoundsException",
                            number,
                            units.len(),
                            span,
                        ));
                    }
                }
            }
            // Groovy's `getAt(Object, String)` reads a property of any other object, like
            // `entry['value']` of a map entry.
            (Value::MapEntry(_) | Value::Object(_), Value::String(name) | Value::GString(name)) => {
                self.property(object, name, span)?
            }
            _ => {
                let class = object.class_name();
                let index = index.class_name();
                return Err(unsupported(format!("indexing {class} with {index}"), span));
            }
        })
    }

    /// `object[index] = value`. A list grows with `null`s; tuples and records reject changes.
    pub(super) fn set_index(
        &mut self,
        object: &Value,
        index: &Value,
        value: Value,
        span: Span,
    ) -> Result<(), Error> {
        match (object, index) {
            (Value::Null, _) => {
                let message = "Cannot invoke method putAt() on null object";
                Err(thrown(
                    Exception::new("NullPointerException", message),
                    span,
                ))
            }
            (Value::Map(map), key) => {
                if map.borrow().kind == MapKind::Record {
                    return Err(rejected(span));
                }
                map.borrow_mut().entries.insert(Key(key.clone()), value);
                Ok(())
            }
            (Value::List(list), Value::Number(number)) if integral(number) => {
                let mut list = list.borrow_mut();
                if matches!(list.kind, ListKind::Tuple | ListKind::Range) {
                    return Err(rejected(span));
                }
                let length = list.items.len();
                let Some(position) = position(number.to_i64(), length) else {
                    return Err(out_of_bounds(
                        "ArrayIndexOutOfBoundsException",
                        number,
                        length,
                        span,
                    ));
                };
                if position >= length {
                    if list.kind == ListKind::Array {
                        return Err(out_of_bounds(
                            "ArrayIndexOutOfBoundsException",
                            number,
                            length,
                            span,
                        ));
                    }
                    list.items.resize(position + 1, Value::Null);
                }
                list.items[position] = value;
                Ok(())
            }
            _ => {
                let class = object.class_name();
                Err(unsupported(format!("setting an index of {class}"), span))
            }
        }
    }

    /// The value of a key that a map lacks: null, or for a map from `withDefault { }` the
    /// closure's value for the key, which the map keeps.
    pub(super) fn default_value(
        &mut self,
        map: &Rc<std::cell::RefCell<crate::value::Map>>,
        key: &Value,
        span: Span,
    ) -> Result<Value, Error> {
        let Some(default) = map.borrow().default.clone() else {
            return Ok(Value::Null);
        };
        let value = self.call_closure_value(&default, vec![key.clone()], span)?;
        map.borrow_mut()
            .entries
            .insert(Key(key.clone()), value.clone());
        Ok(value)
    }

    /// `object.name(arguments)`. Only `call` on closures and the core of exceptions so far.
    pub(super) fn call_method(
        &mut self,
        object: &Value,
        name: &str,
        arguments: Vec<Value>,
        span: Span,
    ) -> Result<Value, Error> {
        match (object, name) {
            // Groovy's null object has a few methods of its own.
            (Value::Null, "equals" | "is") if arguments.len() == 1 => {
                Ok(Value::Boolean(matches!(arguments[0], Value::Null)))
            }
            (Value::Null, "toString") if arguments.is_empty() => Ok(Value::string("null")),
            (Value::Null, "hashCode") if arguments.is_empty() => Ok(Value::integer(0)),
            (Value::Null, "asBoolean") if arguments.is_empty() => Ok(Value::Boolean(false)),
            (Value::Null, "getClass") if arguments.is_empty() => Ok(Value::Class(
                "org.codehaus.groovy.runtime.NullObject".into(),
            )),
            (Value::Null, _) => {
                // Groovy iterates null as nothing for these methods of every object.
                let iterates = matches!(
                    name,
                    "findAll"
                        | "collect"
                        | "each"
                        | "eachWithIndex"
                        | "find"
                        | "any"
                        | "every"
                        | "grep"
                        | "inject"
                );
                if iterates
                    && let Some(value) = self.library_method(object, name, &arguments, span)?
                {
                    return Ok(value);
                }
                let message = format!("Cannot invoke method {name}() on null object");
                Err(thrown(
                    Exception::new("NullPointerException", message),
                    span,
                ))
            }
            (Value::Closure(closure), "call") => self.call_closure_value(closure, arguments, span),
            (Value::Object(object), _) if super::classes::is_library(object) => {
                self.library_call(object, name, arguments, span)
            }
            (Value::Namespace(namespace), _) => {
                let namespace = namespace.clone();
                self.namespace_call(&namespace, name, arguments, span)
            }
            (Value::Channel(_) | Value::ChannelOut(_), _) => {
                self.operator(object, name, arguments, span)
            }
            (Value::Exception(exception), "toJSON") if arguments.is_empty() => {
                match org_json::exception_json(exception, span) {
                    Some(json) => json,
                    None => Err(unsupported("the method `toJSON` of this exception", span)),
                }
            }
            (Value::Exception(_), "getMessage" | "getCause") if arguments.is_empty() => {
                let name = if name == "getMessage" {
                    "message"
                } else {
                    "cause"
                };
                self.property(object, name, span)
            }
            _ => {
                if let Some(value) = self.library_method(object, name, &arguments, span)? {
                    return Ok(value);
                }
                // Groovy calls a closure that a map holds under the name of a missing method.
                if let Value::Map(map) = object {
                    let entry = map.borrow().entries.get(&Key(Value::string(name))).cloned();
                    if let Some(Value::Closure(closure)) = entry {
                        return self.call_closure_value(&closure, arguments, span);
                    }
                }
                // A method that Groovy does not have for the class fails as in Groovy.
                let java_class = object.java_class_name();
                if super::library::groovy_methods::has_method(java_class, name) == Some(false) {
                    return Err(super::call::no_signature(
                        java_class, name, &arguments, span,
                    ));
                }
                // Groovy has no path methods for lists, like the files of a task input.
                if matches!(object, Value::List(_)) && PATH_METHODS.contains(&name) {
                    let class = object.java_class_name();
                    return Err(super::call::no_signature(class, name, &arguments, span));
                }
                let class = object.class_name();
                Err(unsupported(format!("the method `{name}` of {class}"), span))
            }
        }
    }

    /// `value as Class` for the conversions of the core; `null` stays `null`.
    pub(super) fn cast(&mut self, value: Value, class: &str, span: Span) -> Result<Value, Error> {
        // A primitive type converts as its boxed class does.
        let class = match class {
            "int" => "Integer",
            "long" => "Long",
            "boolean" => "Boolean",
            "double" => "Double",
            "float" => "Float",
            "char" => "Character",
            other => other,
        };
        if matches!(value, Value::Null) || instance_of(&value, class) && class != "String" {
            return Ok(value);
        }
        // A Java array of the elements, each cast to the class of the array.
        if let (Some(element), Value::List(_) | Value::Set(_)) = (class.strip_suffix("[]"), &value)
        {
            let mut items = Vec::new();
            for item in super::library::elements(&value) {
                items.push(self.cast(item, element, span)?);
            }
            let list = crate::value::List::new(ListKind::Array, items);
            return Ok(Value::List(Rc::new(std::cell::RefCell::new(list))));
        }
        if let Some(converted) = super::library::convert(&value, class) {
            return converted.at(span);
        }
        Ok(match (class, &value) {
            // A character from a one-character text or a code, and a code from a character.
            ("Character", Value::String(text) | Value::GString(text)) => {
                let mut units = text.encode_utf16();
                match (units.next(), units.next()) {
                    (Some(unit), None) => Value::Character(unit),
                    _ => {
                        let message = format!(
                            "Cannot cast object '{text}' with class 'java.lang.String' to class 'char'"
                        );
                        return Err(thrown(Exception::new("ClassCastException", message), span));
                    }
                }
            }
            ("Character", Value::Number(number)) => Value::Character(number.to_i64() as u16),
            ("Integer" | "Long", Value::Character(unit)) => {
                let code = Value::integer(i32::from(*unit));
                return self.cast(code, class, span);
            }
            ("String", _) => Value::string(value.to_groovy_string()),
            ("Boolean", _) => Value::Boolean(value.truth()),
            ("Integer", Value::Number(number)) => Value::integer(number.to_i32()),
            ("Long", Value::Number(number)) => Value::Number(Number::Long(number.to_i64())),
            ("List", Value::Set(_) | Value::List(_)) => {
                Value::list(super::library::elements(&value))
            }
            ("List", Value::String(text) | Value::GString(text)) => {
                Value::list(text.chars().map(|c| Value::string(c.to_string())).collect())
            }
            ("Set", Value::List(_)) => Value::set(super::library::elements(&value)),
            ("Integer" | "Long", Value::String(text) | Value::GString(text)) => {
                let trimmed = text.trim();
                let parsed: Option<i64> = trimmed.parse().ok();
                match (class, parsed) {
                    ("Integer", Some(number)) if i32::try_from(number).is_ok() => {
                        Value::integer(number as i32)
                    }
                    ("Long", Some(number)) => Value::Number(Number::Long(number)),
                    _ => return Err(thrown(Exception::number_format(trimmed), span)),
                }
            }
            _ => {
                let from = value.class_name();
                return Err(unsupported(format!("casting {from} to {class}"), span));
            }
        })
    }

    /// `new Class(arguments)`, so far for exceptions only.
    pub(super) fn new_object(
        &mut self,
        class: &str,
        arguments: Vec<Value>,
        span: Span,
    ) -> Result<Value, Error> {
        if let Some(object) = self.org_json_new(class, &arguments, span)? {
            return Ok(object);
        }
        match (exception::class_name(class), arguments.as_slice()) {
            (Some(class), []) => Ok(Value::Exception(Rc::new(Exception::new(class, "")))),
            (Some(class), [message]) => {
                let exception = Exception::new(class, message.to_string());
                Ok(Value::Exception(Rc::new(exception)))
            }
            (None, []) if class == "Date" => Ok(Value::Date(crate::date::now())),
            (None, [] | [Value::Number(_)])
                if matches!(class, "StringBuilder" | "StringBuffer") =>
            {
                Ok(super::library::builder::builder(class, String::new()))
            }
            (None, [text]) if matches!(class, "StringBuilder" | "StringBuffer") => {
                Ok(super::library::builder::builder(class, text.to_string()))
            }
            // `new BigDecimal(text or number)` and `new BigInteger(text)`, as `toBigDecimal()`
            // and `toBigInteger()` read them.
            (None, [value]) if matches!(class, "BigDecimal" | "BigInteger") => {
                let method = match class {
                    "BigDecimal" => "toBigDecimal",
                    _ => "toBigInteger",
                };
                self.call_method(value, method, Vec::new(), span)
            }
            (None, []) if matches!(class, "LinkedHashMap" | "HashMap" | "TreeMap") => {
                Ok(Value::map([]))
            }
            (None, [Value::Map(map)]) if matches!(class, "LinkedHashMap" | "HashMap") => {
                Ok(super::library::copy_map(&Value::Map(map.clone())))
            }
            (None, []) if class == "HashSet" => Ok(super::library::hash_set(16, &[])),
            // Java sizes the table of `new HashSet(items)` for at least 16.
            (None, [items @ (Value::List(_) | Value::Set(_))]) if class == "HashSet" => {
                let items = super::library::elements(items);
                let capacity = ((items.len() as f64 / 0.75) as usize + 1).max(16);
                Ok(super::library::hash_set(
                    capacity.next_power_of_two(),
                    &items,
                ))
            }
            (None, []) if class == "LinkedHashSet" => Ok(Value::set([])),
            (None, [items @ (Value::List(_) | Value::Set(_))]) if class == "LinkedHashSet" => {
                Ok(Value::set(super::library::elements(items)))
            }
            (None, []) if matches!(class, "ArrayList" | "LinkedList") => {
                Ok(Value::list(Vec::new()))
            }
            (None, [items @ (Value::List(_) | Value::Set(_))]) if class == "ArrayList" => {
                Ok(Value::list(super::library::elements(items)))
            }
            // A relative file is in the launch directory but keeps its text.
            (None, [path]) if class == "File" => {
                let launch = self.host.launch_dir();
                let text = path.to_string();
                let mut file = PathValue::parse(&text, &launch);
                if file.prefix.is_none() && !text.starts_with('/') {
                    file.relative = Some(text);
                }
                Ok(Value::Path(Rc::new(file)))
            }
            (None, [parent, child]) if class == "File" => {
                let launch = self.host.launch_dir();
                let parent = PathValue::parse(&parent.to_string(), &launch);
                Ok(Value::Path(Rc::new(parent.resolve(&child.to_string()))))
            }
            (None, [text]) if class == "URI" => Ok(super::library::uri(&text.to_string())),
            // `new URI(scheme, userInfo, host, port, path, query, fragment)`, where `null` and
            // a port of -1 leave a part out.
            (None, [scheme, user, host, port, path, query, fragment]) if class == "URI" => {
                let part = |value: &Value, before: &str, after: &str| match value {
                    Value::Null => String::new(),
                    value => format!("{before}{value}{after}"),
                };
                let port = match port {
                    Value::Number(port) if port.to_i64() != -1 => format!(":{port}"),
                    _ => String::new(),
                };
                let mut authority = format!("{}{}{port}", part(user, "", "@"), part(host, "", ""));
                if !authority.is_empty() {
                    authority.insert_str(0, "//");
                }
                let text = format!(
                    "{}{authority}{}{}{}",
                    part(scheme, "", ":"),
                    part(path, "", ""),
                    part(query, "?", ""),
                    part(fragment, "#", "")
                );
                Ok(super::library::uri(&text))
            }
            // A `FileReader` reads the file, relative to the launch directory, as a stream.
            (None, [path]) if class == "FileReader" => {
                let file = match path {
                    Value::Path(path) => (**path).clone(),
                    other => PathValue::parse(&other.to_string(), &self.host.launch_dir()),
                };
                let bytes = self.host.read(&file).map_err(|error| error.or_at(span))?;
                Ok(super::library::stream(bytes))
            }
            // Readers read the stream they wrap.
            (None, [stream @ Value::Object(_), ..])
                if matches!(class, "InputStreamReader" | "BufferedReader") =>
            {
                Ok(stream.clone())
            }
            (None, [Value::Object(object)]) if class == "GZIPInputStream" => {
                let State::Stream(bytes, position) = &object.borrow().state else {
                    return Err(unsupported(
                        "a GZIPInputStream of something other than a stream",
                        span,
                    ));
                };
                // As Java reads the header first: nothing is the end of the stream, and other
                // bytes than those of gzip are no gzip.
                let rest = &bytes[(*position).min(bytes.len())..];
                if rest.is_empty() {
                    return Err(thrown(Exception::new("EOFException", ""), span));
                }
                if !rest.starts_with(&[0x1f, 0x8b]) {
                    let exception = Exception::new("ZipException", "Not in GZIP format");
                    return Err(thrown(exception, span));
                }
                let mut decoded = Vec::new();
                let mut decoder = flate2::read::MultiGzDecoder::new(rest);
                std::io::Read::read_to_end(&mut decoder, &mut decoded).map_err(|error| {
                    thrown(Exception::new("ZipException", error.to_string()), span)
                })?;
                Ok(super::library::stream(decoded))
            }
            (None, []) if class == "Yaml" => {
                Ok(Value::Object(Rc::new(std::cell::RefCell::new(Object {
                    class: "org.yaml.snakeyaml.Yaml",
                    state: State::Yaml,
                }))))
            }
            (None, []) if class == "YamlSlurper" => {
                Ok(Value::Object(Rc::new(std::cell::RefCell::new(Object {
                    class: "groovy.yaml.YamlSlurper",
                    state: State::Yaml,
                }))))
            }
            (None, []) if class == "ConfigSlurper" => {
                Ok(Value::Object(Rc::new(std::cell::RefCell::new(Object {
                    class: "groovy.util.ConfigSlurper",
                    state: State::ConfigSlurper,
                }))))
            }
            // Both engines render the templates of nf-core pipelines the same way.
            (None, []) if class == "SimpleTemplateEngine" || class == "GStringTemplateEngine" => {
                Ok(Value::Object(Rc::new(std::cell::RefCell::new(Object {
                    class: match class {
                        "SimpleTemplateEngine" => "groovy.text.SimpleTemplateEngine",
                        _ => "groovy.text.GStringTemplateEngine",
                    },
                    state: State::TemplateEngine,
                }))))
            }
            (None, []) if class == "JsonGenerator.Options" || class == "JsonSlurper" => {
                let (class, state) = match class {
                    "JsonSlurper" => ("groovy.json.JsonSlurper", State::JsonSlurper),
                    _ => (
                        "groovy.json.JsonGenerator$Options",
                        State::Json(Default::default()),
                    ),
                };
                Ok(Value::Object(Rc::new(std::cell::RefCell::new(Object {
                    class,
                    state,
                }))))
            }
            // A `JsonBuilder` of a value is known by its JSON text, which `toString()` gives.
            (None, [value]) if class == "JsonBuilder" => {
                let text = self.json_text(value, &Default::default(), span)?;
                Ok(Value::Object(Rc::new(std::cell::RefCell::new(Object {
                    class: "groovy.json.JsonBuilder",
                    state: State::Text(text.into()),
                }))))
            }
            (None, [Value::Number(millis)]) if class == "Date" => Ok(Value::Date(millis.to_i64())),
            _ => Err(unsupported(format!("`new {class}(...)`"), span)),
        }
    }
}

/// Methods of paths, from Nextflow's `FilesEx` and the JDK, that no list has.
const PATH_METHODS: &[&str] = &[
    "exists",
    "getBaseName",
    "getExtension",
    "getFileName",
    "getName",
    "getParent",
    "getScheme",
    "getSimpleName",
    "getText",
    "isAbsolute",
    "isDirectory",
    "isFile",
    "readLines",
    "resolve",
    "resolveSibling",
    "toAbsolutePath",
    "toRealPath",
    "toUriString",
];

/// `value instanceof Class`, by the simple class name and its supertypes.
pub(crate) fn instance_of(value: &Value, class: &str) -> bool {
    let own = value.class_name();
    let supertypes: &[&str] = match value {
        Value::Null => return false,
        Value::Boolean(_) => &["Comparable"],
        Value::Number(_) => &["Number", "Comparable"],
        Value::Character(_) => &["Comparable"],
        Value::String(_) => &["CharSequence", "Comparable"],
        Value::GString(_) => &["GString", "CharSequence", "Comparable"],
        Value::List(list) if list.borrow().kind == ListKind::Array => &[],
        Value::List(list) if list.borrow().kind == ListKind::Range => {
            &["Range", "List", "Collection", "Iterable"]
        }
        Value::List(list) if list.borrow().kind == ListKind::Tuple => {
            &["ArrayList", "List", "Collection", "Iterable"]
        }
        Value::List(_) => &["List", "Collection", "Iterable"],
        Value::Set(_) => &["HashSet", "Set", "Collection", "Iterable"],
        Value::Map(map) if matches!(map.borrow().kind, MapKind::Record | MapKind::Config) => {
            &["LinkedHashMap", "Map"]
        }
        Value::Map(_) => &["Map"],
        Value::MemoryUnit(_) | Value::Duration(_) | Value::Date(_) => &["Comparable"],
        Value::Exception(exception) => return class == "Object" || exception.is_instance(class),
        Value::Path(_) => &["Path", "Comparable"],
        Value::Channel(_) | Value::ChannelOut(_) => &["Channel"],
        Value::Definition(_) | Value::Namespace(_) | Value::Operator(_) => &[],
        Value::MapEntry(_)
        | Value::Closure(_)
        | Value::Class(_)
        | Value::Object(_)
        | Value::Pattern(_)
        | Value::Matcher(_) => &[],
    };
    class == "Object" || class == own || supertypes.contains(&class)
}

/// The part of a list that an integer range selects, as Groovy's `IntRange.subListBorders`:
/// negative ends count from the end, and a descending range gives the elements reversed.
fn borders(indexes: &List, size: usize, span: Span) -> Result<(usize, usize, bool), Error> {
    let bounds = indexes
        .range
        .as_ref()
        .and_then(|range| match (&range.from, &range.to) {
            (Value::Number(Number::Integer(from)), Value::Number(Number::Integer(to))) => {
                Some((i64::from(*from), i64::from(*to), range.exclusive))
            }
            _ => None,
        });
    let Some((from, to, exclusive)) = bounds else {
        return Err(unsupported(
            "indexing with a range that is not of integers",
            span,
        ));
    };
    let size = size as i64;
    let from = if from < 0 { from + size } else { from };
    let to = if to < 0 { to + size } else { to };
    let (start, end, reverse) = if from > to {
        (if exclusive { to + 1 } else { to }, from + 1, true)
    } else {
        (from, if exclusive { to } else { to + 1 }, false)
    };
    // Java's `List.subList` checks, with its messages.
    let failure = if start < 0 {
        Some(("IndexOutOfBoundsException", format!("fromIndex = {start}")))
    } else if end > size {
        Some(("IndexOutOfBoundsException", format!("toIndex = {end}")))
    } else if start > end {
        Some((
            "IllegalArgumentException",
            format!("fromIndex({start}) > toIndex({end})"),
        ))
    } else {
        None
    };
    if let Some((class, message)) = failure {
        return Err(thrown(Exception::new(class, message), span));
    }
    Ok((start as usize, end as usize, reverse))
}

/// Whether a number is an index: `Integer`, `Long` or `BigInteger`.
fn integral(number: &Number) -> bool {
    matches!(
        number,
        Number::Integer(_) | Number::Long(_) | Number::BigInteger(_)
    )
}

/// A list position, counting a negative index from the end; `None` before the start.
fn position(index: i64, length: usize) -> Option<usize> {
    let position = if index < 0 {
        length as i64 + index
    } else {
        index
    };
    usize::try_from(position).ok()
}

fn out_of_bounds(class: &'static str, index: &Number, length: usize, span: Span) -> Error {
    let message = format!("Index {index} out of bounds for length {length}");
    thrown(Exception::new(class, message), span)
}

/// The `UnsupportedOperationException` of a tuple or record that rejects a change.
fn rejected(span: Span) -> Error {
    thrown(Exception::new("UnsupportedOperationException", ""), span)
}
