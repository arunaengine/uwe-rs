//! Running the Groovy classes of a pipeline's `lib/` directory, which only the legacy parser
//! accepts: static fields and methods, enums, and objects with fields, methods and
//! constructors.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::rc::Rc;

use indexmap::IndexMap;
use nextflow_parser::Span;

use super::{Error, Frame, Interpreter, thrown};
use crate::exception::Exception;
use crate::lowered::{LibraryClass, MethodDef};
use crate::value::{List, ListKind, Object, State, Value};

/// Whether an object stands for a class of the library, one of its enum constants or one of
/// its objects.
pub(super) fn is_library(object: &Rc<RefCell<Object>>) -> bool {
    matches!(
        object.borrow().state,
        State::Class(_) | State::EnumConstant { .. } | State::Instance { .. }
    )
}

/// The Java name of a class of the library, which error messages and `getClass()` show: a
/// nested class follows its outer class after `$`.
fn java_name(class: &LibraryClass) -> String {
    class.def.name.replace('.', "$")
}

impl Interpreter<'_> {
    /// The class of the library that code names, as a value.
    pub(super) fn library_class(
        &mut self,
        frame: &Frame,
        name: &str,
        span: Span,
    ) -> Result<Value, Error> {
        let library = frame.module.library.borrow().clone();
        let class = library.and_then(|library| library.classes.borrow().get(name).cloned());
        match class {
            Some(class) => Ok(class_value(&class)),
            None => {
                let message = format!("No such property: {name} for class: Script");
                Err(thrown(
                    Exception::new("MissingPropertyException", message),
                    span,
                ))
            }
        }
    }

    /// `new Name(arguments)` for a class of the library of the script, or `None`.
    pub(super) fn library_new(
        &mut self,
        frame: &Frame,
        name: &str,
        arguments: Vec<Value>,
        span: Span,
    ) -> Result<Option<Value>, Error> {
        let library = frame.module.library.borrow().clone();
        let class = library.and_then(|library| library.classes.borrow().get(name).cloned());
        let Some(class) = class else {
            return Ok(None);
        };
        self.initialize(&class, span)?;
        let object = Rc::new(RefCell::new(Object {
            class: "groovy.lang.GroovyObject",
            state: State::Instance {
                class: class.clone(),
                fields: IndexMap::new(),
            },
        }));
        let instance = Value::Object(object.clone());
        for field in class.def.fields.iter().filter(|field| !field.is_static) {
            let value = match &field.value {
                Some(code) => self.run(code, &class.module, Vec::new(), vec![instance.clone()])?,
                None => Value::Null,
            };
            let value = self.convert(value, field.class.as_deref(), span)?;
            if let State::Instance { fields, .. } = &mut object.borrow_mut().state {
                fields.insert(field.name.clone(), value);
            }
        }
        let constructors: Vec<&MethodDef> = class.def.constructors.iter().collect();
        if constructors.is_empty() {
            // Groovy's constructor with named properties, like `new Point(x: 1)`.
            match arguments.as_slice() {
                [] => {}
                [Value::Map(map)] => {
                    let entries: Vec<(Value, Value)> = map
                        .borrow()
                        .entries
                        .iter()
                        .map(|(key, value)| (key.0.clone(), value.clone()))
                        .collect();
                    for (key, value) in entries {
                        self.set_library_property(&object, &key.to_string(), value, span)?;
                    }
                }
                _ => return Err(no_method(&class, "<init>", &arguments, span)),
            }
            return Ok(Some(instance));
        }
        let Some((method, arguments)) = choose(&constructors, arguments.clone(), true) else {
            return Err(no_method(&class, "<init>", &arguments, span));
        };
        let mut all = vec![instance.clone()];
        all.extend(arguments);
        self.run(&method.code, &class.module, Vec::new(), all)?;
        Ok(Some(instance))
    }

    /// `object.name` on a class, an enum constant or an object of the library.
    pub(super) fn library_property(
        &mut self,
        object: &Rc<RefCell<Object>>,
        name: &str,
        span: Span,
    ) -> Result<Value, Error> {
        let state = library_state(object);
        match state {
            Library::Class(class) => {
                self.initialize(&class, span)?;
                if let Some(value) = static_field(&class, name) {
                    return Ok(value);
                }
                let nested = format!("{}.{name}", class.def.name);
                if class.def.nested.iter().any(|inner| **inner == *nested) {
                    let library = class.module.library.borrow().clone();
                    let inner = library
                        .and_then(|library| library.classes.borrow().get(nested.as_str()).cloned());
                    if let Some(inner) = inner {
                        return Ok(class_value(&inner));
                    }
                }
                match name {
                    "name" => Ok(Value::string(java_name(&class))),
                    "simpleName" => Ok(Value::string(
                        class.def.name.rsplit('.').next().unwrap_or_default(),
                    )),
                    _ => Err(no_property(&java_name(&class), name, span)),
                }
            }
            Library::Constant(class, constant, ordinal) => match name {
                "name" => Ok(Value::string(constant)),
                "ordinal" => Ok(Value::integer(ordinal as i32)),
                _ => Err(no_property(&java_name(&class), name, span)),
            },
            Library::Instance(class) => {
                let field = match &object.borrow().state {
                    State::Instance { fields, .. } => fields.get(name).cloned(),
                    _ => None,
                };
                if let Some(value) = field {
                    return Ok(value);
                }
                let getter = format!("get{}", capitalized(name));
                if class
                    .def
                    .methods
                    .iter()
                    .any(|method| *method.name == *getter)
                {
                    return self.library_call(object, &getter, Vec::new(), span);
                }
                self.initialize(&class, span)?;
                match static_field(&class, name) {
                    Some(value) => Ok(value),
                    None => Err(no_property(&java_name(&class), name, span)),
                }
            }
        }
    }

    /// `object.name = value` on a class or an object of the library.
    pub(super) fn set_library_property(
        &mut self,
        object: &Rc<RefCell<Object>>,
        name: &str,
        value: Value,
        span: Span,
    ) -> Result<(), Error> {
        match library_state(object) {
            Library::Class(class) => {
                self.initialize(&class, span)?;
                let declared = class
                    .def
                    .fields
                    .iter()
                    .find(|f| f.is_static && *f.name == *name);
                let Some(declared) = declared else {
                    return Err(no_property(&java_name(&class), name, span));
                };
                let value = self.convert(value, declared.class.as_deref(), span)?;
                if let Some(statics) = class.statics.borrow_mut().as_mut() {
                    statics.insert(name.into(), value);
                }
                Ok(())
            }
            Library::Constant(class, ..) => Err(no_property(&java_name(&class), name, span)),
            Library::Instance(class) => {
                let declared = class.def.fields.iter().find(|f| *f.name == *name);
                let converted = match declared {
                    Some(declared) => self.convert(value, declared.class.as_deref(), span)?,
                    None => return Err(no_property(&java_name(&class), name, span)),
                };
                if let State::Instance { fields, .. } = &mut object.borrow_mut().state {
                    fields.insert(name.into(), converted);
                }
                Ok(())
            }
        }
    }
}
