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
            yields: function.generator.then(|| RefCell::new(Vec::new())),
            method,
        };
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
        let result = result?;
        match frame.yields {
            Some(yields) => Ok(Value::iterator(yields.into_inner())),
            None => Ok(result),
        }
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
                if crate::modules::is_native(&module.name) {
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
        let value = value.plain();
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
}
