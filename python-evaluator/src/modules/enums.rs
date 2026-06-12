//! `enum`: `Enum`, `IntEnum`, `StrEnum`, `auto` and `unique`. The classes are written in
//! Python; what Python's metaclass of enumerations does is here: the members of a class
//! that derives from `Enum`, and iterating, indexing, sizing and searching the class.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::rc::Rc;

use indexmap::IndexMap;

use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::value::{Class, Dict, Instance, Key, Module, Value};

const SOURCE: &str = r#"
class Enum:
    def __new__(cls, value):
        for member in cls._member_map_.values():
            if member._value_ == value:
                return member
        raise ValueError(f"{value!r} is not a valid {cls.__qualname__}")

    def __repr__(self):
        return f"<{type(self).__name__}.{self._name_}: {self._value_!r}>"

    def __str__(self):
        return f"{type(self).__name__}.{self._name_}"


class IntEnum(Enum):
    def __eq__(self, other):
        return self is other or self._value_ == other

    def __hash__(self):
        return hash(self._value_)

    def __int__(self):
        return self._value_

    def __str__(self):
        return str(self._value_)


class StrEnum(Enum):
    def __eq__(self, other):
        return self is other or self._value_ == other

    def __hash__(self):
        return hash(self._value_)

    def __str__(self):
        return self._value_


class auto:
    pass


def unique(enumeration):
    return enumeration
"#;

/// The module `enum`, whose classes the Python source above defines.
pub fn module(interpreter: &mut Interpreter) -> Result<Value, Error> {
    let globals = interpreter.new_globals("enum", None);
    interpreter.exec_text("<enum>", SOURCE, &globals)?;
    Ok(Value::Module(Rc::new(Module {
        name: "enum".into(),
        globals,
    })))
}

/// Whether a class derives from `enum.Enum`.
fn is_enumeration(class: &Rc<Class>) -> bool {
    class
        .mro
        .borrow()
        .iter()
        .skip(1)
        .any(|base| &*base.module == "enum" && &*base.name == "Enum")
}

/// Whether a value of a class body is a member, as Python's enumerations take them: data,
/// not functions, methods or properties.
fn is_member_value(value: &Value) -> bool {
    match value.plain() {
        Value::Instance(instance) => {
            &*instance.class.module == "enum" && &*instance.class.name == "auto"
        }
        Value::Function(_)
        | Value::Builtin(_)
        | Value::Method(_)
        | Value::Class(_)
        | Value::Module(_)
        | Value::Native(_) => false,
        _ => true,
    }
}

/// Turns the data of a new class that derives from `Enum` into its members: instances of
/// the class with `name` and `value`, in `_member_map_`, `__members__` and the class.
pub fn make_members(class: &Rc<Class>) -> Result<(), Error> {
    if !is_enumeration(class) {
        return Ok(());
    }
    let entries: Vec<(Rc<str>, Value)> = class
        .dict
        .borrow()
        .iter()
        .filter(|(name, value)| !name.starts_with('_') && is_member_value(value))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    let mut members = Dict::default();
    let mut last = 0;
    for (name, value) in entries {
        // `auto()` numbers the members from 1 on, after the last whole number.
        let value = match &value {
            Value::Instance(_) => Value::Int(last + 1),
            other => other.clone(),
        };
        if let Value::Int(number) = value {
            last = number;
        }
        let mut dict = IndexMap::new();
        dict.insert("_name_".into(), Value::str(&*name));
        dict.insert("_value_".into(), value.clone());
        dict.insert("name".into(), Value::str(&*name));
        dict.insert("value".into(), value);
        let member = Value::Instance(Rc::new(Instance {
            class: class.clone(),
            dict: RefCell::new(dict),
        }));
        members
            .entries
            .insert(Key::new(Value::str(&*name))?, member.clone());
        class.dict.borrow_mut().insert(name, member);
    }
    let members = Value::dict(members);
    let mut dict = class.dict.borrow_mut();
    dict.insert("_member_map_".into(), members.clone());
    dict.insert("__members__".into(), members);
    Ok(())
}

/// The members of a class that derives from `Enum`, or `None` for another class.
pub fn members(class: &Rc<Class>) -> Option<Vec<Value>> {
    if !is_enumeration(class) {
        return None;
    }
    match class.lookup("_member_map_")? {
        Value::Dict(members) => Some(members.borrow().entries.values().cloned().collect()),
        _ => None,
    }
}

/// `Class["NAME"]`: the member of that name.
pub fn member(class: &Rc<Class>, key: &Value) -> Option<Result<Value, Error>> {
    members(class)?;
    let Some(Value::Dict(members)) = class.lookup("_member_map_") else {
        return None;
    };
    let found = Key::new(key.clone())
        .ok()
        .and_then(|key| members.borrow().entries.get(&key).cloned());
    Some(found.ok_or_else(|| Error::key_error(key.clone())))
}
