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

    /// `object.name(arguments)` on a class, an enum constant or an object of the library.
    pub(super) fn library_call(
        &mut self,
        object: &Rc<RefCell<Object>>,
        name: &str,
        arguments: Vec<Value>,
        span: Span,
    ) -> Result<Value, Error> {
        let this = Value::Object(object.clone());
        match library_state(object) {
            Library::Class(class) => {
                self.initialize(&class, span)?;
                let methods: Vec<&MethodDef> = class
                    .def
                    .methods
                    .iter()
                    .filter(|method| method.is_static && *method.name == *name)
                    .collect();
                if let Some((method, arguments)) = choose(&methods, arguments.clone(), false) {
                    let result = self.run(&method.code, &class.module, Vec::new(), arguments)?;
                    return self.convert(result, method.returns.as_deref(), span);
                }
                self.class_builtin(&class, name, &arguments, span)
            }
            Library::Constant(class, constant, ordinal) => match (name, arguments.as_slice()) {
                ("name" | "toString", []) => Ok(Value::string(constant)),
                ("ordinal", []) => Ok(Value::integer(ordinal as i32)),
                ("hashCode", []) => Ok(Value::integer(this.java_hash())),
                ("equals", [other]) => Ok(Value::Boolean(this.java_equals(other))),
                ("compareTo", [Value::Object(other)]) => match library_state(other) {
                    Library::Constant(_, _, other) => {
                        Ok(Value::integer(ordinal as i32 - other as i32))
                    }
                    _ => Err(no_method(&class, name, &arguments, span)),
                },
                ("getDeclaringClass" | "getClass", []) => Ok(class_value(&class)),
                _ => Err(no_method(&class, name, &arguments, span)),
            },
            Library::Instance(class) => {
                self.initialize(&class, span)?;
                let methods: Vec<&MethodDef> = class
                    .def
                    .methods
                    .iter()
                    .filter(|method| *method.name == *name)
                    .collect();
                let instance: Vec<&MethodDef> =
                    methods.iter().copied().filter(|m| !m.is_static).collect();
                if let Some((method, arguments)) = choose(&instance, arguments.clone(), true) {
                    let mut all = vec![this.clone()];
                    all.extend(arguments);
                    let result = self.run(&method.code, &class.module, Vec::new(), all)?;
                    return self.convert(result, method.returns.as_deref(), span);
                }
                let statics: Vec<&MethodDef> =
                    methods.iter().copied().filter(|m| m.is_static).collect();
                if let Some((method, arguments)) = choose(&statics, arguments.clone(), false) {
                    let result = self.run(&method.code, &class.module, Vec::new(), arguments)?;
                    return self.convert(result, method.returns.as_deref(), span);
                }
                match (name, arguments.as_slice()) {
                    ("getClass", []) => Ok(class_value(&class)),
                    ("hashCode", []) => Ok(Value::integer(this.java_hash())),
                    ("equals", [other]) => Ok(Value::Boolean(this.java_equals(other))),
                    ("toString", []) => Ok(Value::string(this.to_string())),
                    _ => Err(no_method(&class, name, &arguments, span)),
                }
            }
        }
    }

    /// The methods that every class has, like `values()` of an enum.
    fn class_builtin(
        &mut self,
        class: &Rc<LibraryClass>,
        name: &str,
        arguments: &[Value],
        span: Span,
    ) -> Result<Value, Error> {
        let constants = || match &*class.values.borrow() {
            Some((constants, _)) => constants.clone(),
            None => Vec::new(),
        };
        match (name, arguments) {
            ("values", []) if class.def.enumeration => {
                let list = List::new(ListKind::Array, constants());
                Ok(Value::List(Rc::new(RefCell::new(list))))
            }
            ("valueOf", [text]) if class.def.enumeration => {
                let text = text.to_string();
                let found = constants().into_iter().find(|constant| match constant {
                    Value::Object(object) => {
                        matches!(&object.borrow().state, State::EnumConstant { name, .. } if **name == *text)
                    }
                    _ => false,
                });
                found.ok_or_else(|| {
                    let message = format!("No enum constant {}.{text}", java_name(class));
                    thrown(Exception::illegal_argument(message), span)
                })
            }
            ("getName", []) => Ok(Value::string(java_name(class))),
            ("getSimpleName", []) => Ok(Value::string(
                class.def.name.rsplit('.').next().unwrap_or_default(),
            )),
            _ => Err(no_method(class, name, arguments, span)),
        }
    }

    /// Sets the static fields of a class the first time it is used, in order, as Java does.
    /// A field that an initializer reads before it is set is still `null`.
    fn initialize(&mut self, class: &Rc<LibraryClass>, span: Span) -> Result<(), Error> {
        if class.statics.borrow().is_some() {
            return Ok(());
        }
        *class.statics.borrow_mut() = Some(IndexMap::new());
        class_value(class);
        for field in class.def.fields.iter().filter(|field| field.is_static) {
            let value = match &field.value {
                Some(code) => self.run(code, &class.module, Vec::new(), Vec::new())?,
                None => Value::Null,
            };
            let value = self.convert(value, field.class.as_deref(), span)?;
            if let Some(statics) = class.statics.borrow_mut().as_mut() {
                statics.insert(field.name.clone(), value);
            }
        }
        Ok(())
    }

    /// A value converted to a declared type, as Groovy converts what a typed field is given or
    /// a typed method returns; other types keep the value.
    fn convert(&mut self, value: Value, class: Option<&str>, span: Span) -> Result<Value, Error> {
        match class {
            Some("void") => Ok(Value::Null),
            Some(
                class @ ("String" | "Integer" | "int" | "Long" | "long" | "Double" | "double"
                | "Boolean" | "boolean" | "BigDecimal" | "Float" | "float"),
            ) if !matches!(value, Value::Null) => self.cast(value, class, span),
            _ => Ok(value),
        }
    }
}

/// What an object of the library stands for.
enum Library {
    Class(Rc<LibraryClass>),
    Constant(Rc<LibraryClass>, Rc<str>, usize),
    Instance(Rc<LibraryClass>),
}

fn library_state(object: &Rc<RefCell<Object>>) -> Library {
    match &object.borrow().state {
        State::Class(class) => Library::Class(class.clone()),
        State::EnumConstant {
            class,
            name,
            ordinal,
        } => Library::Constant(class.clone(), name.clone(), *ordinal),
        State::Instance { class, .. } => Library::Instance(class.clone()),
        _ => unreachable!("an object of the library"),
    }
}

/// The class as a value, with its enum constants, each made once.
fn class_value(class: &Rc<LibraryClass>) -> Value {
    if let Some((_, value)) = &*class.values.borrow() {
        return value.clone();
    }
    let value = Value::Object(Rc::new(RefCell::new(Object {
        class: "java.lang.Class",
        state: State::Class(class.clone()),
    })));
    let constants = class
        .def
        .constants
        .iter()
        .enumerate()
        .map(|(ordinal, name)| {
            Value::Object(Rc::new(RefCell::new(Object {
                class: "java.lang.Enum",
                state: State::EnumConstant {
                    class: class.clone(),
                    name: name.clone(),
                    ordinal,
                },
            })))
        })
        .collect();
    *class.values.borrow_mut() = Some((constants, value.clone()));
    value
}

/// The value of a static field or enum constant, if the class has one of that name.
fn static_field(class: &Rc<LibraryClass>, name: &str) -> Option<Value> {
    if let Some(value) = class
        .statics
        .borrow()
        .as_ref()
        .and_then(|statics| statics.get(name).cloned())
    {
        return Some(value);
    }
    if class
        .def
        .fields
        .iter()
        .any(|f| f.is_static && *f.name == *name)
    {
        return Some(Value::Null);
    }
    let index = class.def.constants.iter().position(|c| **c == *name)?;
    class_value(class);
    let values = class.values.borrow();
    values
        .as_ref()
        .map(|(constants, _)| constants[index].clone())
}

/// The method that Groovy chooses for the arguments, with the arguments it takes: the right
/// number, remaining arguments collected for a method with variable arguments, and of the
/// methods whose parameter types fit, the one with the most typed parameters. A method of the
/// objects has `this` as its first parameter, which the arguments leave out.
fn choose<'m>(
    methods: &[&'m MethodDef],
    arguments: Vec<Value>,
    instance: bool,
) -> Option<(&'m MethodDef, Vec<Value>)> {
    let mut best: Option<(&MethodDef, Vec<Value>, usize)> = None;
    for method in methods {
        let skip = usize::from(instance && !method.is_static);
        let parameters = &method.code.parameters[skip..];
        let required = parameters
            .iter()
            .filter(|parameter| parameter.default.is_none())
            .count();
        let adapted = if method.varargs {
            let fixed = parameters.len() - 1;
            if arguments.len() < fixed.min(required) {
                continue;
            }
            let mut adapted: Vec<Value> = arguments.iter().take(fixed).cloned().collect();
            let rest: Vec<Value> = arguments.iter().skip(fixed).cloned().collect();
            let list = List::new(ListKind::Array, rest);
            adapted.push(Value::List(Rc::new(RefCell::new(list))));
            adapted
        } else {
            if arguments.len() < required || arguments.len() > parameters.len() {
                continue;
            }
            arguments.clone()
        };
        let checked = method.types.iter().zip(&adapted);
        let mut score = 0;
        let mut fits = true;
        for (class, value) in checked {
            if let Some(class) = class {
                if !fits_type(class, value) {
                    fits = false;
                    break;
                }
                score += 1;
            }
        }
        if fits && best.as_ref().is_none_or(|(_, _, best)| score > *best) {
            best = Some((method, adapted, score));
        }
    }
    best.map(|(method, arguments, _)| (method, arguments))
}

/// Whether a value fits a declared parameter type; a type iunctus does not know fits all.
fn fits_type(class: &str, value: &Value) -> bool {
    if matches!(value, Value::Null) {
        return !matches!(
            class,
            "int" | "long" | "boolean" | "double" | "float" | "short" | "byte" | "char"
        );
    }
    match class {
        "Map" | "LinkedHashMap" | "HashMap" => matches!(value, Value::Map(_)),
        "List" | "ArrayList" | "Collection" => matches!(value, Value::List(_)),
        "Set" => matches!(value, Value::Set(_)),
        "String" | "CharSequence" => matches!(value, Value::String(_) | Value::GString(_)),
        "Integer" | "int" | "Long" | "long" | "Number" | "Double" | "double" | "BigDecimal"
        | "Float" | "float" => matches!(value, Value::Number(_)),
        "Boolean" | "boolean" => matches!(value, Value::Boolean(_)),
        "Path" | "File" => matches!(value, Value::Path(_)),
        "Closure" => matches!(value, Value::Closure(_)),
        _ => true,
    }
}

fn capitalized(name: &str) -> String {
    let mut characters = name.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().chain(characters).collect(),
        None => String::new(),
    }
}

/// Groovy's `MissingMethodException` for a method of a class of the library.
fn no_method(class: &LibraryClass, name: &str, arguments: &[Value], span: Span) -> Error {
    let types: Vec<&str> = arguments.iter().map(Value::class_name).collect();
    let values: Vec<String> = arguments.iter().map(Value::to_string).collect();
    let message = format!(
        "No signature of method: {}.{name}() is applicable for argument types: ({}) values: [{}]",
        java_name(class),
        types.join(", "),
        values.join(", ")
    );
    thrown(Exception::new("MissingMethodException", message), span)
}

fn no_property(class: &str, name: &str, span: Span) -> Error {
    let message = format!("No such property: {name} for class: {class}");
    thrown(Exception::new("MissingPropertyException", message), span)
}
