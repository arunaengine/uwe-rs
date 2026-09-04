//! Calls with Python's argument binding, attribute access, subscripts and iteration.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::rc::Rc;

use indexmap::IndexMap;

use super::{Frame, FrameKind, Interpreter, bind_method, statement::Flow};
use crate::error::Error;
use crate::types::{self, TypeTag};
use crate::value::{Args, Code, Dict, Function, Instance, Iter, Key, MethodFunction, Scope, Value};

impl Interpreter {
    /// Calls a callable value.
    pub fn call(&mut self, callee: &Value, args: Args) -> Result<Value, Error> {
        match callee.plain() {
            Value::Function(function) => self.call_function(function, args, None),
            Value::Builtin(builtin) => {
                let function = builtin.function.clone();
                function(self, args)
            }
            Value::Method(method) => match &method.function {
                MethodFunction::Value(function) => {
                    let mut args = args;
                    args.positional.insert(0, method.receiver.clone());
                    if let Value::Function(inner) = function
                        && let Value::Instance(instance) = method.receiver.plain()
                    {
                        let class = defining_class(&instance.class, inner);
                        return self.call_function(
                            inner,
                            args,
                            class.map(|class| (class, method.receiver.clone())),
                        );
                    }
                    self.call(function, args)
                }
                MethodFunction::Builtin(name) => {
                    let receiver = method.receiver.clone();
                    let name = name.clone();
                    crate::methods::call(self, &receiver, &name, args)
                }
            },
            Value::Class(class) => self.instantiate(class, args),
            Value::Instance(_) => match self.lookup_special(callee, "__call__") {
                Some(method) => self.call(&method, args),
                None => Err(Error::type_error(format!(
                    "'{}' object is not callable",
                    types::type_name(callee)
                ))),
            },
            Value::Native(object) => {
                let object = object.clone();
                match object.call(self, callee.plain(), args)? {
                    Some(result) => Ok(result),
                    None => Err(Error::type_error(format!(
                        "'{}' object is not callable",
                        object.type_name()
                    ))),
                }
            }
            other => Err(Error::type_error(format!(
                "'{}' object is not callable",
                types::type_name(other)
            ))),
        }
    }

    /// Whether a value can be called.
    pub fn callable(&mut self, value: &Value) -> bool {
        match value.plain() {
            Value::Function(_) | Value::Builtin(_) | Value::Method(_) | Value::Class(_) => true,
            Value::Instance(_) => self.lookup_special(value, "__call__").is_some(),
            Value::Native(object) => object.instance_of("callable"),
            _ => false,
        }
    }

    /// Calls a function written in Python.
    pub fn call_function(
        &mut self,
        function: &Rc<Function>,
        args: Args,
        method: Option<(Rc<crate::value::Class>, Value)>,
    ) -> Result<Value, Error> {
        let scope = Scope::new(function.closure.clone());
        self.bind_arguments(function, &scope, args)?;
        let frame = Frame {
            locals: scope,
            globals: function.globals.clone(),
            source: function.source.clone(),
            kind: FrameKind::Function,
            declared: function.declared.clone(),
            yields: None,
            method,
        };
        if function.generator {
            let Code::Def(def) = &function.code else {
                return Err(Error::unsupported("yield in lambda"));
            };
            return Ok(Value::Iterator(Rc::new(RefCell::new(Iter::Generator(
                Box::new(super::generator::Generator::new(frame, &def.body)),
            )))));
        }
        self.enter_call()?;
        let result = match &function.code {
            Code::Lambda(lambda) => self.eval(&frame, &lambda.body),
            Code::Def(def) => match self.exec_suite(&frame, &def.body) {
                Ok(Flow::Return(value)) => Ok(value),
                Ok(_) => Ok(Value::None),
                Err(error) => Err(error),
            },
        };
        self.leave_call();
        result
    }

    fn bind_arguments(
        &mut self,
        function: &Rc<Function>,
        scope: &Rc<Scope>,
        args: Args,
    ) -> Result<(), Error> {
        let name = &function.name;
        let Some(parameters) = function.code.parameters() else {
            if !args.is_empty() {
                return Err(Error::type_error(format!(
                    "{name}() takes 0 positional arguments but {} were given",
                    args.positional.len()
                )));
            }
            return Ok(());
        };
        let positional_parameters: Vec<&str> = parameters
            .posonlyargs
            .iter()
            .chain(parameters.args.iter())
            .map(|parameter| parameter.parameter.name.id.as_str())
            .collect();
        let mut bound: IndexMap<&str, Value> = IndexMap::new();
        let mut extra = Vec::new();
        for (index, value) in args.positional.into_iter().enumerate() {
            match positional_parameters.get(index) {
                Some(parameter) => {
                    bound.insert(parameter, value);
                }
                None => extra.push(value),
            }
        }
        if let Some(vararg) = &parameters.vararg {
            scope.set(vararg.name.id.as_str(), Value::tuple(extra));
        } else if !extra.is_empty() {
            return Err(Error::type_error(format!(
                "{name}() takes {} positional arguments but {} were given",
                positional_parameters.len(),
                positional_parameters.len() + extra.len()
            )));
        }
        let keyword_names: Vec<&str> = parameters
            .args
            .iter()
            .chain(parameters.kwonlyargs.iter())
            .map(|parameter| parameter.parameter.name.id.as_str())
            .collect();
        let mut kwargs = Dict::default();
        for (key, value) in args.keywords {
            if let Some(parameter) = keyword_names.iter().find(|name| ***name == *key) {
                if bound.contains_key(parameter) {
                    return Err(Error::type_error(format!(
                        "{name}() got multiple values for argument '{key}'"
                    )));
                }
                bound.insert(parameter, value);
            } else if parameters.kwarg.is_some() {
                kwargs.entries.insert(Key(Value::Str(key)), value);
            } else {
                return Err(Error::type_error(format!(
                    "{name}() got an unexpected keyword argument '{key}'"
                )));
            }
        }
        if let Some(kwarg) = &parameters.kwarg {
            scope.set(kwarg.name.id.as_str(), Value::dict(kwargs));
        }
        let all = parameters
            .posonlyargs
            .iter()
            .chain(parameters.args.iter())
            .chain(parameters.kwonlyargs.iter());
        for parameter in all {
            let parameter_name = parameter.parameter.name.id.as_str();
            let value = match bound.shift_remove(parameter_name) {
                Some(value) => value,
                None => match function.defaults.get(parameter_name) {
                    Some(default) => default.clone(),
                    None => {
                        return Err(Error::type_error(format!(
                            "{name}() missing 1 required positional argument: '{parameter_name}'"
                        )));
                    }
                },
            };
            scope.set(parameter_name, value);
        }
        Ok(())
    }

    /// Calls a class: a builtin constructor, an exception, or a new object of a Python class.
    fn instantiate(&mut self, class: &Rc<crate::value::Class>, args: Args) -> Result<Value, Error> {
        if let Some(tag) = class.builtin
            && tag != TypeTag::BaseException
            && class.module.as_ref() == "builtins"
        {
            return crate::builtins::construct(self, tag, args);
        }
        if let Some(new) = class.lookup("__new__")
            && !matches!(class.builtin, Some(TypeTag::BaseException))
            && !matches!(new, Value::Builtin(_))
        {
            let mut new_args = args.clone();
            new_args.positional.insert(0, Value::Class(class.clone()));
            let new = match new {
                Value::Native(ref object) if object.type_name() == "staticmethod" => {
                    bind_method(&Value::None, new.clone())
                }
                other => other,
            };
            let object = self.call(&new, new_args)?;
            if let Value::Instance(_) = object
                && let Some(init) = self.lookup_special(&object, "__init__")
            {
                self.call(&init, args)?;
            }
            return Ok(object);
        }
        let mut dict = IndexMap::new();
        if class.builtin == Some(TypeTag::BaseException) {
            dict.insert("args".into(), Value::tuple(args.positional.clone()));
        }
        let object = Value::Instance(Rc::new(Instance {
            class: class.clone(),
            dict: RefCell::new(dict),
        }));
        if let Some(init) = class.lookup("__init__") {
            let init = bind_method(&object, init);
            self.call(&init, args)?;
        } else if class.builtin != Some(TypeTag::BaseException) && !args.is_empty() {
            return Err(Error::type_error(format!(
                "{}() takes no arguments",
                class.name
            )));
        }
        Ok(object)
    }

    /// `value.name`.
    pub fn get_attribute(&mut self, value: &Value, name: &str) -> Result<Value, Error> {
        match self.find_attribute(value, name)? {
            Some(found) => Ok(found),
            None => Err(self.missing_attribute(value, name)),
        }
    }

    fn missing_attribute(&self, value: &Value, name: &str) -> Error {
        match value.plain() {
            Value::Module(module) => {
                let full = format!("{}.{name}", module.name);
                if crate::modules::is_native(&module.name)
                    && !crate::modules::is_removed(&module.name, name)
                {
                    Error::unsupported(format!("the library function {full}"))
                } else {
                    Error::attribute_error(format!(
                        "module '{}' has no attribute '{name}'",
                        module.name
                    ))
                }
            }
            Value::Class(class) => Error::attribute_error(format!(
                "type object '{}' has no attribute '{name}'",
                class.name
            )),
            Value::Instance(_) | Value::Function(_) => Error::attribute_error(format!(
                "'{}' object has no attribute '{name}'",
                types::type_name(value)
            )),
            Value::Native(object) => {
                Error::unsupported(format!("the attribute {name} of {}", object.type_name()))
            }
            other => {
                if crate::methods::python_has(other, name) {
                    Error::unsupported(format!("{}.{name}", types::type_name(other)))
                } else {
                    Error::attribute_error(format!(
                        "'{}' object has no attribute '{name}'",
                        types::type_name(other)
                    ))
                }
            }
        }
    }

    /// `value.name`, or `None` when the object has no such attribute.
    pub fn find_attribute(&mut self, value: &Value, name: &str) -> Result<Option<Value>, Error> {
        let found = self.find_plain_attribute(value.plain(), name)?;
        if found.is_none()
            && let Value::Tagged(_) = value
            && let Some(hook) = self.attribute_hook.clone()
        {
            return hook.attribute(self, value, name);
        }
        Ok(found)
    }

    /// The attribute of a value without its flags.
    fn find_plain_attribute(&mut self, value: &Value, name: &str) -> Result<Option<Value>, Error> {
        if name == "__class__" {
            return Ok(Some(Value::Class(types::class_of(value))));
        }
        match value {
            Value::Instance(instance) => {
                if let Some(found) = instance.dict.borrow().get(name) {
                    return Ok(Some(found.clone()));
                }
                if name == "__dict__" {
                    let dict = instance.dict.borrow().clone();
                    return Ok(Some(Value::dict_of(
                        dict.into_iter()
                            .map(|(key, value)| (key.to_string(), value)),
                    )));
                }
                if let Some(found) = instance.class.lookup(name) {
                    if let Value::Native(object) = &found
                        && object.type_name() == "property"
                    {
                        let getter = object
                            .as_any()
                            .downcast_ref::<crate::builtins::Wrapper>()
                            .map(|wrapper| wrapper.0.clone())
                            .unwrap_or(Value::None);
                        return self.call(&getter, Args::new(vec![value.clone()])).map(Some);
                    }
                    return Ok(Some(bind_method(value, found)));
                }
                if types::is_exception(value) && name == "args" {
                    return Ok(Some(Value::tuple(Vec::new())));
                }
                if let Some(getattr) = instance.class.lookup("__getattr__") {
                    let getattr = bind_method(value, getattr);
                    return match self.call(&getattr, Args::new(vec![Value::str(name)])) {
                        Ok(found) => Ok(Some(found)),
                        Err(error) if error.is("AttributeError") => Ok(None),
                        Err(error) => Err(error),
                    };
                }
                Ok(None)
            }
            Value::Class(class) => {
                if let Some(found) = class.lookup(name) {
                    return Ok(Some(match &found {
                        Value::Native(object)
                            if matches!(object.type_name(), "classmethod" | "staticmethod") =>
                        {
                            bind_method(value, found.clone())
                        }
                        _ => found,
                    }));
                }
                Ok(match name {
                    "__name__" | "__qualname__" => Some(Value::Str(class.name.to_string().into())),
                    "__module__" => Some(Value::Str(class.module.to_string().into())),
                    "__mro__" => Some(Value::tuple(
                        class
                            .mro
                            .borrow()
                            .iter()
                            .map(|class| Value::Class(class.clone()))
                            .collect(),
                    )),
                    "__bases__" => Some(Value::tuple(
                        class
                            .bases
                            .iter()
                            .map(|class| Value::Class(class.clone()))
                            .collect(),
                    )),
                    "__doc__" => Some(Value::None),
                    _ => crate::builtins::class_attribute(self, class, name)?,
                })
            }
            Value::Module(module) => Ok(module.globals.get(name).or_else(|| {
                (name == "__name__").then(|| Value::Str(module.name.to_string().into()))
            })),
            Value::Function(function) => {
                if let Some(found) = function.attributes.borrow().get(name) {
                    return Ok(Some(found.clone()));
                }
                Ok(match name {
                    "__name__" => Some(Value::Str(function.name.clone())),
                    "__qualname__" => Some(Value::Str(function.qualname.clone())),
                    "__module__" => Some(function.globals.get("__name__").unwrap_or(Value::None)),
                    "__doc__" => Some(docstring(function)),
                    "__defaults__" => Some(Value::None),
                    "__code__" => Some(crate::builtins::code_object(function)),
                    "__globals__" => Some(Value::None),
                    "__dict__" => Some(Value::dict(Dict::default())),
                    _ => None,
                })
            }
            Value::Builtin(builtin) => Ok(match name {
                "__name__" | "__qualname__" => Some(Value::Str(builtin.name.clone())),
                "__doc__" => Some(Value::None),
                _ => None,
            }),
            Value::Method(method) => Ok(match name {
                "__self__" => Some(method.receiver.clone()),
                "__func__" => match &method.function {
                    MethodFunction::Value(function) => Some(function.clone()),
                    MethodFunction::Builtin(_) => None,
                },
                "__name__" => Some(match &method.function {
                    MethodFunction::Value(function) => Value::Str(super::name_of(function)),
                    MethodFunction::Builtin(name) => Value::Str(name.clone()),
                }),
                _ => match &method.function {
                    MethodFunction::Value(function) => self.find_attribute(function, name)?,
                    MethodFunction::Builtin(_) => None,
                },
            }),
            Value::Native(object) => {
                let object = object.clone();
                object.get_attribute(self, value, name)
            }
            other => crate::methods::attribute(self, other, name),
        }
    }

    /// `object.name = value`.
    pub fn set_attribute(&mut self, object: &Value, name: &str, value: Value) -> Result<(), Error> {
        match object.plain() {
            Value::Instance(instance) => {
                if let Some(Value::Native(property)) = instance.class.lookup(name)
                    && property.type_name() == "property"
                {
                    return Err(Error::attribute_error(format!(
                        "property '{name}' of '{}' object has no setter",
                        instance.class.name
                    )));
                }
                instance.dict.borrow_mut().insert(name.into(), value);
                Ok(())
            }
            Value::Class(class) => {
                class.dict.borrow_mut().insert(name.into(), value);
                Ok(())
            }
            Value::Function(function) => {
                function.attributes.borrow_mut().insert(name.into(), value);
                Ok(())
            }
            Value::Module(module) => {
                module.globals.set(name, value);
                Ok(())
            }
            Value::Native(native) => {
                let native = native.clone();
                if native.set_attribute(self, name, value)? {
                    Ok(())
                } else {
                    Err(Error::unsupported(format!(
                        "setting the attribute {name} of {}",
                        native.type_name()
                    )))
                }
            }
            other => Err(Error::attribute_error(format!(
                "'{}' object has no attribute '{name}'",
                types::type_name(other)
            ))),
        }
    }

    /// `value[key]`.
    pub fn get_item(&mut self, value: &Value, key: &Value) -> Result<Value, Error> {
        let key = key.plain();
        match value.plain() {
            Value::List(items) => {
                let items = items.borrow();
                sequence_item(&items, key, |items| Value::list(items.to_vec()), "list")
            }
            Value::Tuple(items) => {
                sequence_item(items, key, |items| Value::tuple(items.to_vec()), "tuple")
            }
            Value::Str(text) => {
                if text.is_ascii() {
                    let bytes: Vec<Value> = Vec::new();
                    let _ = bytes;
                    let length = text.len() as i64;
                    match key {
                        Value::Slice(_) => {
                            let indexes = slice_indexes(key, length)?;
                            let bytes = text.as_bytes();
                            let picked: String = indexes
                                .into_iter()
                                .map(|index| bytes[index] as char)
                                .collect();
                            Ok(Value::str(picked))
                        }
                        _ => {
                            let index = normalize_index(key, length, "string")?;
                            Ok(Value::str(&text[index..index + 1]))
                        }
                    }
                } else {
                    let chars: Vec<char> = text.chars().collect();
                    let length = chars.len() as i64;
                    match key {
                        Value::Slice(_) => {
                            let indexes = slice_indexes(key, length)?;
                            Ok(Value::str(
                                indexes
                                    .into_iter()
                                    .map(|index| chars[index])
                                    .collect::<String>(),
                            ))
                        }
                        _ => {
                            let index = normalize_index(key, length, "string")?;
                            Ok(Value::str(chars[index].to_string()))
                        }
                    }
                }
            }
            Value::Bytes(bytes) => match key {
                Value::Slice(_) => {
                    let indexes = slice_indexes(key, bytes.len() as i64)?;
                    Ok(Value::Bytes(
                        indexes.into_iter().map(|index| bytes[index]).collect(),
                    ))
                }
                _ => {
                    let index = normalize_index(key, bytes.len() as i64, "index")?;
                    Ok(Value::Int(i64::from(bytes[index])))
                }
            },
            Value::Range(range) => match key {
                Value::Slice(_) => {
                    let indexes = slice_indexes(key, range.len() as i64)?;
                    Ok(Value::list(
                        indexes
                            .into_iter()
                            .map(|index| Value::Int(range.get(index)))
                            .collect(),
                    ))
                }
                _ => {
                    let index = normalize_index(key, range.len() as i64, "range object")?;
                    Ok(Value::Int(range.get(index)))
                }
            },
            Value::Dict(dict) => {
                let lookup = Key::new(key.clone())?;
                if let Some(found) = dict.borrow().entries.get(&lookup) {
                    return Ok(found.clone());
                }
                let kind = dict.borrow().kind.clone();
                match kind {
                    crate::value::DictKind::Default(factory) if !factory.is_none() => {
                        let made = self.call(&factory, Args::default())?;
                        dict.borrow_mut().entries.insert(lookup, made.clone());
                        Ok(made)
                    }
                    crate::value::DictKind::Counter => Ok(Value::Int(0)),
                    _ => Err(Error::key_error(key.clone())),
                }
            }
            Value::Instance(_) => match self.lookup_special(value, "__getitem__") {
                Some(method) => self.call(&method, Args::new(vec![key.clone()])),
                None => Err(Error::type_error(format!(
                    "'{}' object is not subscriptable",
                    types::type_name(value)
                ))),
            },
            Value::Class(class) => match crate::modules::enums::member(class, key) {
                Some(member) => member,
                // Generic aliases like `list[str]` in annotations.
                None => Ok(value.plain().clone()),
            },
            Value::Native(object) => {
                let object = object.clone();
                match object.get_item(self, value.plain(), key)? {
                    Some(found) => Ok(found),
                    None => Err(Error::type_error(format!(
                        "'{}' object is not subscriptable",
                        object.type_name()
                    ))),
                }
            }
            other => Err(Error::type_error(format!(
                "'{}' object is not subscriptable",
                types::type_name(other)
            ))),
        }
    }

    /// `object[key] = value`.
    pub fn set_item(&mut self, object: &Value, key: &Value, value: Value) -> Result<(), Error> {
        let key = key.plain();
        match object.plain() {
            Value::List(items) => {
                if let Value::Slice(_) = key {
                    let replacement = self.iterate(&value)?;
                    let mut items = items.borrow_mut();
                    let length = items.len() as i64;
                    let (start, stop, step) = slice_bounds(key, length)?;
                    if step != 1 {
                        let indexes = slice_indexes(key, length)?;
                        if indexes.len() != replacement.len() {
                            return Err(Error::value_error(format!(
                                "attempt to assign sequence of size {} to extended slice of size {}",
                                replacement.len(),
                                indexes.len()
                            )));
                        }
                        for (index, item) in indexes.into_iter().zip(replacement) {
                            items[index] = item;
                        }
                        return Ok(());
                    }
                    let start = start.clamp(0, length) as usize;
                    let stop = (stop.clamp(0, length) as usize).max(start);
                    items.splice(start..stop, replacement);
                    return Ok(());
                }
                let mut items = items.borrow_mut();
                let length = items.len() as i64;
                let index = normalize_index(key, length, "list assignment")?;
                items[index] = value;
                Ok(())
            }
            Value::Dict(dict) => {
                let key = Key::new(key.clone())?;
                dict.borrow_mut().entries.insert(key, value);
                Ok(())
            }
            Value::Instance(_) => match self.lookup_special(object, "__setitem__") {
                Some(method) => {
                    self.call(&method, Args::new(vec![key.clone(), value]))?;
                    Ok(())
                }
                None => Err(Error::type_error(format!(
                    "'{}' object does not support item assignment",
                    types::type_name(object)
                ))),
            },
            Value::Native(native) => {
                let native = native.clone();
                if native.set_item(self, key, value)? {
                    Ok(())
                } else {
                    Err(Error::unsupported(format!(
                        "item assignment on {}",
                        native.type_name()
                    )))
                }
            }
            other => Err(Error::type_error(format!(
                "'{}' object does not support item assignment",
                types::type_name(other)
            ))),
        }
    }

    /// `del object[key]`.
    pub fn delete_item(&mut self, object: &Value, key: &Value) -> Result<(), Error> {
        let key = key.plain();
        match object.plain() {
            Value::List(items) => {
                let mut items = items.borrow_mut();
                let length = items.len() as i64;
                if let Value::Slice(_) = key {
                    let mut indexes = slice_indexes(key, length)?;
                    indexes.sort_unstable();
                    for index in indexes.into_iter().rev() {
                        items.remove(index);
                    }
                    return Ok(());
                }
                let index = normalize_index(key, length, "list assignment")?;
                items.remove(index);
                Ok(())
            }
            Value::Dict(dict) => {
                let lookup = Key::new(key.clone())?;
                if dict.borrow_mut().entries.shift_remove(&lookup).is_none() {
                    return Err(Error::key_error(key.clone()));
                }
                Ok(())
            }
            Value::Instance(_) => match self.lookup_special(object, "__delitem__") {
                Some(method) => {
                    self.call(&method, Args::new(vec![key.clone()]))?;
                    Ok(())
                }
                None => Err(Error::type_error("object does not support item deletion")),
            },
            other => Err(Error::type_error(format!(
                "'{}' object does not support item deletion",
                types::type_name(other)
            ))),
        }
    }

    /// The items of an iterable, all at once.
    pub fn iterate(&mut self, value: &Value) -> Result<Vec<Value>, Error> {
        if let Value::Class(class) = value.plain()
            && let Some(members) = crate::modules::enums::members(class)
        {
            return Ok(members);
        }
        match value.plain() {
            Value::List(items) => Ok(items.borrow().clone()),
            Value::Tuple(items) => Ok(items.to_vec()),
            Value::Str(text) => Ok(text.chars().map(|c| Value::str(c.to_string())).collect()),
            Value::Bytes(bytes) => Ok(bytes
                .iter()
                .map(|byte| Value::Int(i64::from(*byte)))
                .collect()),
            Value::Dict(dict) => Ok(dict
                .borrow()
                .entries
                .keys()
                .map(|key| key.0.clone())
                .collect()),
            Value::Set(set) => Ok(set.borrow().iter().map(|key| key.0.clone()).collect()),
            Value::FrozenSet(set) => Ok(set.iter().map(|key| key.0.clone()).collect()),
            Value::Range(range) => {
                if range.len() > 100_000_000 {
                    return Err(Error::unsupported(
                        "iterating over more than 100000000 numbers",
                    ));
                }
                Ok((0..range.len())
                    .map(|index| Value::Int(range.get(index)))
                    .collect())
            }
            Value::Iterator(_) => {
                let mut items = Vec::new();
                loop {
                    let next = self.next_item(value)?;
                    match next {
                        Some(item) => items.push(item),
                        None => break,
                    }
                    if items.len() > 100_000_000 {
                        return Err(Error::unsupported("an iterator that does not end"));
                    }
                }
                Ok(items)
            }
            Value::Instance(_) => {
                if let Some(method) = self.lookup_special(value, "__iter__") {
                    let iterator = self.call(&method, Args::default())?;
                    if iterator.is(value) {
                        // An object that is its own iterator: call `__next__` until it stops.
                        let next = self
                            .lookup_special(value, "__next__")
                            .ok_or_else(|| Error::type_error("iter() returned non-iterator"))?;
                        let mut items = Vec::new();
                        loop {
                            match self.call(&next, Args::default()) {
                                Ok(item) => items.push(item),
                                Err(error) if error.is("StopIteration") => break,
                                Err(error) => return Err(error),
                            }
                        }
                        return Ok(items);
                    }
                    return self.iterate(&iterator);
                }
                if let Some(method) = self.lookup_special(value, "__getitem__") {
                    let mut items = Vec::new();
                    loop {
                        match self.call(&method, Args::new(vec![Value::Int(items.len() as i64)])) {
                            Ok(item) => items.push(item),
                            Err(error) if error.is("IndexError") || error.is("StopIteration") => {
                                break;
                            }
                            Err(error) => return Err(error),
                        }
                    }
                    return Ok(items);
                }
                Err(Error::type_error(format!(
                    "'{}' object is not iterable",
                    types::type_name(value)
                )))
            }
            Value::Native(object) => {
                let object = object.clone();
                match object.iterate(self)? {
                    Some(items) => Ok(items),
                    None => Err(Error::type_error(format!(
                        "'{}' object is not iterable",
                        object.type_name()
                    ))),
                }
            }
            other => Err(Error::type_error(format!(
                "'{}' object is not iterable",
                types::type_name(other)
            ))),
        }
    }

    /// Advances an iterator without retaining its borrow across item consumption.
    pub fn next_item(&mut self, value: &Value) -> Result<Option<Value>, Error> {
        let Value::Iterator(iterator) = value.plain() else {
            return Err(Error::type_error("expected an iterator"));
        };
        let mut state = iterator
            .try_borrow_mut()
            .map_err(|_| Error::value_error("generator already executing"))?;
        state.next_value(self)
    }

    fn protocol_iterator(&mut self, value: &Value) -> Result<Value, Error> {
        let next = match value.plain() {
            Value::Iterator(_) => return Ok(value.plain().clone()),
            Value::Instance(_) => self.lookup_special(value, "__next__"),
            Value::Native(object) => object.clone().get_attribute(self, value, "__next__")?,
            _ => None,
        };
        match next {
            Some(next) => Ok(Value::Iterator(Rc::new(RefCell::new(Iter::Protocol(next))))),
            None => Err(Error::type_error("iter() returned non-iterator")),
        }
    }

    /// `iter(value)`: an iterator that yields items one by one.
    pub fn make_iterator(&mut self, value: &Value) -> Result<Value, Error> {
        match value.plain() {
            Value::Iterator(_) => Ok(value.plain().clone()),
            Value::List(items) => Ok(Value::Iterator(Rc::new(RefCell::new(Iter::List {
                items: items.clone(),
                index: 0,
                exhausted: false,
            })))),
            Value::Instance(_) => {
                if let Some(method) = self.lookup_special(value, "__iter__") {
                    let iterator = self.call(&method, Args::default())?;
                    return self.protocol_iterator(&iterator);
                }
                if let Some(get) = self.lookup_special(value, "__getitem__") {
                    return Ok(Value::Iterator(Rc::new(RefCell::new(Iter::Sequence {
                        get,
                        index: 0,
                        exhausted: false,
                    }))));
                }
                Err(Error::type_error(format!(
                    "'{}' object is not iterable",
                    types::type_name(value)
                )))
            }
            Value::Native(object) => {
                let object = object.clone();
                if let Some(method) = object.get_attribute(self, value, "__iter__")? {
                    let iterator = self.call(&method, Args::default())?;
                    return self.protocol_iterator(&iterator);
                }
                if let Some(next) = object.get_attribute(self, value, "__next__")? {
                    return Ok(Value::Iterator(Rc::new(RefCell::new(Iter::Protocol(next)))));
                }
                Ok(Value::iterator(self.iterate(value)?))
            }
            Value::Range(range) => Ok(Value::Iterator(Rc::new(RefCell::new(Iter::Range {
                next: range.start,
                stop: range.stop,
                step: range.step,
            })))),
            other => {
                let items = self.iterate(other)?;
                Ok(Value::iterator(items))
            }
        }
    }

    /// `len(value)`.
    pub fn length(&mut self, value: &Value) -> Result<usize, Error> {
        if let Value::Class(class) = value.plain()
            && let Some(members) = crate::modules::enums::members(class)
        {
            return Ok(members.len());
        }
        Ok(match value.plain() {
            Value::Str(text) => {
                if text.is_ascii() {
                    text.len()
                } else {
                    text.chars().count()
                }
            }
            Value::Bytes(bytes) => bytes.len(),
            Value::List(items) => items.borrow().len(),
            Value::Tuple(items) => items.len(),
            Value::Dict(dict) => dict.borrow().entries.len(),
            Value::Set(set) => set.borrow().len(),
            Value::FrozenSet(set) => set.len(),
            Value::Range(range) => range.len(),
            Value::Instance(_) => match self.lookup_special(value, "__len__") {
                Some(method) => {
                    let length = self.call(&method, Args::default())?;
                    length.as_int().unwrap_or(0).max(0) as usize
                }
                None => {
                    return Err(Error::type_error(format!(
                        "object of type '{}' has no len()",
                        types::type_name(value)
                    )));
                }
            },
            Value::Native(object) => match object.length() {
                Some(length) => length,
                None => {
                    let object = object.clone();
                    match object.iterate(self)? {
                        Some(items) => items.len(),
                        None => {
                            return Err(Error::type_error(format!(
                                "object of type '{}' has no len()",
                                object.type_name()
                            )));
                        }
                    }
                }
            },
            other => {
                return Err(Error::type_error(format!(
                    "object of type '{}' has no len()",
                    types::type_name(other)
                )));
            }
        })
    }
}

/// The class in whose body a method was defined, for `super()`.
fn defining_class(
    class: &Rc<crate::value::Class>,
    function: &Rc<Function>,
) -> Option<Rc<crate::value::Class>> {
    class.mro.borrow().iter().find_map(|candidate| {
        let dict = candidate.dict.borrow();
        dict.values()
            .any(|value| matches!(value, Value::Function(defined) if Rc::ptr_eq(defined, function)))
            .then(|| candidate.clone())
    })
}

fn docstring(function: &Function) -> Value {
    if let Code::Def(def) = &function.code
        && let Some(ruff_python_ast::Stmt::Expr(expression)) = def.body.first()
        && let ruff_python_ast::Expr::StringLiteral(text) = &*expression.value
    {
        return Value::str(text.value.to_str());
    }
    Value::None
}

/// A list or tuple index as a position, counting negative indexes from the end.
pub fn normalize_index(key: &Value, length: i64, what: &str) -> Result<usize, Error> {
    let index = match key.plain() {
        Value::Int(index) => *index,
        Value::Bool(value) => i64::from(*value),
        Value::BigInt(_) => {
            return Err(Error::index_error(format!("{what} index out of range")));
        }
        other => {
            return Err(Error::type_error(format!(
                "{what} indices must be integers or slices, not {}",
                types::type_name(other)
            )));
        }
    };
    let position = if index < 0 { index + length } else { index };
    if position < 0 || position >= length {
        return Err(Error::index_error(format!("{what} index out of range")));
    }
    Ok(position as usize)
}

/// Start, stop and step of a slice for a sequence length, as `slice.indices` gives them.
pub fn slice_bounds(slice: &Value, length: i64) -> Result<(i64, i64, i64), Error> {
    let Value::Slice(parts) = slice.plain() else {
        return Err(Error::type_error("expected a slice"));
    };
    let number = |value: &Value| -> Result<Option<i64>, Error> {
        match value.plain() {
            Value::None => Ok(None),
            other => other.as_int().map(Some).ok_or_else(|| {
                Error::type_error(
                    "slice indices must be integers or None or have an __index__ method",
                )
            }),
        }
    };
    let step = number(&parts[2])?.unwrap_or(1);
    if step == 0 {
        return Err(Error::value_error("slice step cannot be zero"));
    }
    let clamp = |value: Option<i64>, default: i64| -> i64 {
        match value {
            None => default,
            Some(value) => {
                let value = if value < 0 { value + length } else { value };
                if step > 0 {
                    value.clamp(0, length)
                } else {
                    value.clamp(-1, length - 1)
                }
            }
        }
    };
    let (start, stop) = if step > 0 {
        (
            clamp(number(&parts[0])?, 0),
            clamp(number(&parts[1])?, length),
        )
    } else {
        (
            clamp(number(&parts[0])?, length - 1),
            clamp(number(&parts[1])?, -1),
        )
    };
    Ok((start, stop, step))
}

/// The positions a slice picks from a sequence of the given length.
pub fn slice_indexes(slice: &Value, length: i64) -> Result<Vec<usize>, Error> {
    let (start, stop, step) = slice_bounds(slice, length)?;
    let mut indexes = Vec::new();
    let mut index = start;
    while (step > 0 && index < stop) || (step < 0 && index > stop) {
        indexes.push(index as usize);
        index += step;
    }
    Ok(indexes)
}

fn sequence_item(
    items: &[Value],
    key: &Value,
    make: impl Fn(&[Value]) -> Value,
    what: &str,
) -> Result<Value, Error> {
    match key {
        Value::Slice(_) => {
            let indexes = slice_indexes(key, items.len() as i64)?;
            let picked: Vec<Value> = indexes
                .into_iter()
                .map(|index| items[index].clone())
                .collect();
            Ok(make(&picked))
        }
        _ => {
            let index = normalize_index(key, items.len() as i64, what)?;
            Ok(items[index].clone())
        }
    }
}
