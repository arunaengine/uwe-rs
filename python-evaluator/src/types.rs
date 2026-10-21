//! The builtin types and exception classes, shared by all interpreters of a thread.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use indexmap::IndexMap;

use crate::value::{Class, DictKind, Name, Value};

/// Which builtin type a class is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TypeTag {
    Object,
    Type,
    NoneType,
    Bool,
    Int,
    Float,
    Str,
    Bytes,
    List,
    Tuple,
    Dict,
    OrderedDict,
    DefaultDict,
    Counter,
    Set,
    FrozenSet,
    Range,
    Slice,
    Function,
    BuiltinFunction,
    Method,
    Module,
    Iterator,
    Ellipsis,
    NotImplemented,
    BaseException,
    Complex,
}

const TYPES: &[(TypeTag, &str, Option<TypeTag>)] = &[
    (TypeTag::Object, "object", None),
    (TypeTag::Type, "type", Some(TypeTag::Object)),
    (TypeTag::NoneType, "NoneType", Some(TypeTag::Object)),
    (TypeTag::Int, "int", Some(TypeTag::Object)),
    (TypeTag::Bool, "bool", Some(TypeTag::Int)),
    (TypeTag::Float, "float", Some(TypeTag::Object)),
    (TypeTag::Complex, "complex", Some(TypeTag::Object)),
    (TypeTag::Str, "str", Some(TypeTag::Object)),
    (TypeTag::Bytes, "bytes", Some(TypeTag::Object)),
    (TypeTag::List, "list", Some(TypeTag::Object)),
    (TypeTag::Tuple, "tuple", Some(TypeTag::Object)),
    (TypeTag::Dict, "dict", Some(TypeTag::Object)),
    (TypeTag::OrderedDict, "OrderedDict", Some(TypeTag::Dict)),
    (TypeTag::DefaultDict, "defaultdict", Some(TypeTag::Dict)),
    (TypeTag::Counter, "Counter", Some(TypeTag::Dict)),
    (TypeTag::Set, "set", Some(TypeTag::Object)),
    (TypeTag::FrozenSet, "frozenset", Some(TypeTag::Object)),
    (TypeTag::Range, "range", Some(TypeTag::Object)),
    (TypeTag::Slice, "slice", Some(TypeTag::Object)),
    (TypeTag::Function, "function", Some(TypeTag::Object)),
    (
        TypeTag::BuiltinFunction,
        "builtin_function_or_method",
        Some(TypeTag::Object),
    ),
    (TypeTag::Method, "method", Some(TypeTag::Object)),
    (TypeTag::Module, "module", Some(TypeTag::Object)),
    (TypeTag::Iterator, "iterator", Some(TypeTag::Object)),
    (TypeTag::Ellipsis, "ellipsis", Some(TypeTag::Object)),
    (
        TypeTag::NotImplemented,
        "NotImplementedType",
        Some(TypeTag::Object),
    ),
    (
        TypeTag::BaseException,
        "BaseException",
        Some(TypeTag::Object),
    ),
];

/// The builtin exception classes with their base class.
const EXCEPTIONS: &[(&str, &str)] = &[
    ("SystemExit", "BaseException"),
    ("KeyboardInterrupt", "BaseException"),
    ("GeneratorExit", "BaseException"),
    ("Exception", "BaseException"),
    ("StopIteration", "Exception"),
    ("ArithmeticError", "Exception"),
    ("ZeroDivisionError", "ArithmeticError"),
    ("OverflowError", "ArithmeticError"),
    ("FloatingPointError", "ArithmeticError"),
    ("AssertionError", "Exception"),
    ("AttributeError", "Exception"),
    ("BufferError", "Exception"),
    ("EOFError", "Exception"),
    ("ImportError", "Exception"),
    ("ModuleNotFoundError", "ImportError"),
    ("LookupError", "Exception"),
    ("IndexError", "LookupError"),
    ("KeyError", "LookupError"),
    ("MemoryError", "Exception"),
    ("NameError", "Exception"),
    ("UnboundLocalError", "NameError"),
    ("OSError", "Exception"),
    ("FileNotFoundError", "OSError"),
    ("FileExistsError", "OSError"),
    ("PermissionError", "OSError"),
    ("IsADirectoryError", "OSError"),
    ("NotADirectoryError", "OSError"),
    ("TimeoutError", "OSError"),
    ("ReferenceError", "Exception"),
    ("RuntimeError", "Exception"),
    ("NotImplementedError", "RuntimeError"),
    ("RecursionError", "RuntimeError"),
    ("SyntaxError", "Exception"),
    ("SystemError", "Exception"),
    ("TypeError", "Exception"),
    ("ValueError", "Exception"),
    ("UnicodeError", "ValueError"),
    ("UnicodeDecodeError", "UnicodeError"),
    ("UnicodeEncodeError", "UnicodeError"),
    ("Warning", "Exception"),
    ("UserWarning", "Warning"),
    ("DeprecationWarning", "Warning"),
    ("PendingDeprecationWarning", "Warning"),
    ("RuntimeWarning", "Warning"),
    ("FutureWarning", "Warning"),
    ("SyntaxWarning", "Warning"),
    ("ImportWarning", "Warning"),
    ("ResourceWarning", "Warning"),
];

struct Registry {
    types: HashMap<TypeTag, Rc<Class>>,
    exceptions: HashMap<&'static str, Rc<Class>>,
    library: RefCell<HashMap<String, Rc<Class>>>,
    natives: RefCell<HashMap<String, Rc<Class>>>,
}

thread_local! {
    static REGISTRY: Registry = Registry::new();
}

impl Registry {
    fn new() -> Registry {
        let mut types: HashMap<TypeTag, Rc<Class>> = HashMap::new();
        for (tag, name, base) in TYPES {
            let bases: Vec<Rc<Class>> = base.iter().map(|base| types[base].clone()).collect();
            let class = new_class(name, "builtins", bases, Some(*tag));
            types.insert(*tag, class);
        }
        let mut exceptions: HashMap<&'static str, Rc<Class>> = HashMap::new();
        exceptions.insert("BaseException", types[&TypeTag::BaseException].clone());
        for (name, base) in EXCEPTIONS {
            let base = exceptions[base].clone();
            let class = new_class(name, "builtins", vec![base], Some(TypeTag::BaseException));
            exceptions.insert(name, class);
        }
        Registry {
            types,
            exceptions,
            library: RefCell::new(HashMap::new()),
            natives: RefCell::new(HashMap::new()),
        }
    }
}

/// A class with its method resolution order: the class, then the ancestors depth first
/// from the left, each at its last place.
pub fn new_class(
    name: &str,
    module: &str,
    bases: Vec<Rc<Class>>,
    builtin: Option<TypeTag>,
) -> Rc<Class> {
    let builtin = builtin.or_else(|| bases.iter().find_map(|base| base.builtin));
    let class = Rc::new(Class {
        name: name.into(),
        module: module.into(),
        bases: bases.clone(),
        mro: RefCell::new(Vec::new()),
        dict: RefCell::new(IndexMap::new()),
        builtin,
    });
    let mut order: Vec<Rc<Class>> = Vec::new();
    for base in &bases {
        for ancestor in base.mro.borrow().iter() {
            order.retain(|seen| !Rc::ptr_eq(seen, ancestor));
            order.push(ancestor.clone());
        }
    }
    // `object` comes last, as in C3 linearization.
    if let Some(index) = order.iter().position(|class| &*class.name == "object") {
        let object = order.remove(index);
        order.push(object);
    }
    order.insert(0, class.clone());
    *class.mro.borrow_mut() = order;
    class
}

/// The class of a builtin type.
pub fn builtin_class(tag: TypeTag) -> Rc<Class> {
    REGISTRY.with(|registry| registry.types[&tag].clone())
}

/// A builtin or library exception class by name, like `ValueError` or `JSONDecodeError`.
pub fn exception_class(name: &str) -> Option<Rc<Class>> {
    REGISTRY.with(|registry| {
        registry
            .exceptions
            .get(name)
            .cloned()
            .or_else(|| registry.library.borrow().get(name).cloned())
    })
}

/// The exception class of a library module, made once per thread, like `json.JSONDecodeError`.
pub fn library_exception(name: &str, module: &str, base: &str) -> Rc<Class> {
    if let Some(class) = REGISTRY.with(|registry| registry.library.borrow().get(name).cloned()) {
        return class;
    }
    let base =
        exception_class(base).unwrap_or_else(|| exception_class("Exception").expect("exists"));
    let class = new_class(name, module, vec![base], Some(TypeTag::BaseException));
    REGISTRY.with(|registry| {
        registry
            .library
            .borrow_mut()
            .insert(name.to_owned(), class.clone())
    });
    class
}

pub fn exception_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = EXCEPTIONS.iter().map(|(name, _)| *name).collect();
    names.push("BaseException");
    names
}

/// A class that stands for a kind of native object, for `type()` and `isinstance`.
pub fn native_class(name: &str) -> Rc<Class> {
    REGISTRY.with(|registry| {
        registry
            .natives
            .borrow_mut()
            .entry(name.to_owned())
            .or_insert_with(|| {
                new_class(
                    name,
                    "builtins",
                    vec![registry.types[&TypeTag::Object].clone()],
                    None,
                )
            })
            .clone()
    })
}

pub fn class_of(value: &Value) -> Rc<Class> {
    let tag = match value.plain() {
        Value::None => TypeTag::NoneType,
        Value::Bool(_) => TypeTag::Bool,
        Value::Int(_) | Value::BigInt(_) => TypeTag::Int,
        Value::Float(_) => TypeTag::Float,
        Value::Str(_) => TypeTag::Str,
        Value::Bytes(_) => TypeTag::Bytes,
        Value::List(_) => TypeTag::List,
        Value::Tuple(_) => TypeTag::Tuple,
        Value::Dict(dict) => match dict.borrow().kind {
            DictKind::Plain => TypeTag::Dict,
            DictKind::OrderedDict => TypeTag::OrderedDict,
            DictKind::Default(_) => TypeTag::DefaultDict,
            DictKind::Counter => TypeTag::Counter,
        },
        Value::Set(_) => TypeTag::Set,
        Value::FrozenSet(_) => TypeTag::FrozenSet,
        Value::Range(_) => TypeTag::Range,
        Value::Slice(_) => TypeTag::Slice,
        Value::Function(_) => TypeTag::Function,
        Value::Builtin(_) => TypeTag::BuiltinFunction,
        Value::Method(_) => TypeTag::Method,
        Value::Class(_) => TypeTag::Type,
        Value::Module(_) => TypeTag::Module,
        Value::Iterator(_) => TypeTag::Iterator,
        Value::Ellipsis => TypeTag::Ellipsis,
        Value::NotImplemented => TypeTag::NotImplemented,
        Value::Instance(instance) => return instance.class.clone(),
        Value::Native(object) => return native_class(object.type_name()),
        Value::Tagged(_) => unreachable!("plain values are never tagged"),
    };
    builtin_class(tag)
}

/// The name of the type of a value, as error messages show it.
pub fn type_name(value: &Value) -> Name {
    class_of(value).name.clone()
}

/// `isinstance(value, class)`.
pub fn is_instance(value: &Value, class: &Rc<Class>) -> bool {
    if class_of(value).is_subclass(class) {
        return true;
    }
    match value.plain() {
        Value::Native(object) => object.instance_of(&class.name),
        _ => false,
    }
}

/// Whether a value is an exception object.
pub fn is_exception(value: &Value) -> bool {
    matches!(value.plain(), Value::Instance(instance) if instance.class.builtin == Some(TypeTag::BaseException))
}

/// Whether a value is an exception class.
pub fn is_exception_class(value: &Value) -> bool {
    matches!(value.plain(), Value::Class(class) if class.builtin == Some(TypeTag::BaseException))
}

/// `X | Y` of types, like `str | None` in annotations: `isinstance` accepts it.
pub struct UnionType(pub Vec<Value>);

impl UnionType {
    /// Whether a value can be a member of a union: a class, `None`, a native object that
    /// stands for a class, like `pandas.DataFrame`, or another union.
    pub fn is_member(value: &Value) -> bool {
        match value.plain() {
            Value::Class(_) | Value::None => true,
            Value::Native(object) => {
                object.as_any().is::<UnionType>() || object.check_instance(&Value::None).is_some()
            }
            _ => false,
        }
    }

    /// The union of two types, flattening unions within it.
    pub fn of(a: &Value, b: &Value) -> Value {
        let mut members = Vec::new();
        for value in [a, b] {
            match value.as_native::<UnionType>() {
                Some(union) => members.extend(union.0.iter().cloned()),
                None => members.push(value.plain().clone()),
            }
        }
        Value::native(UnionType(members))
    }
}

impl crate::value::Native for UnionType {
    fn type_name(&self) -> &str {
        "UnionType"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn check_instance(&self, value: &Value) -> Option<bool> {
        Some(self.0.iter().any(|member| match member {
            Value::Class(class) => is_instance(value, class),
            Value::None => value.is_none(),
            Value::Native(object) => object.check_instance(value).unwrap_or(false),
            _ => false,
        }))
    }

    fn get_attribute(
        &self,
        _: &mut crate::interpreter::Interpreter,
        _: &Value,
        name: &str,
    ) -> Result<Option<Value>, crate::error::Error> {
        Ok(match name {
            "__args__" => Some(Value::tuple(
                self.0
                    .iter()
                    .map(|member| match member {
                        Value::None => Value::Class(class_of(member)),
                        other => other.clone(),
                    })
                    .collect(),
            )),
            _ => None,
        })
    }

    fn repr(&self, _: &mut crate::interpreter::Interpreter) -> Result<String, crate::error::Error> {
        Ok(self
            .0
            .iter()
            .map(|member| match member {
                Value::Class(class) => class.name.to_string(),
                Value::Native(object) => object.type_name().to_owned(),
                _ => "None".to_owned(),
            })
            .collect::<Vec<_>>()
            .join(" | "))
    }
}
