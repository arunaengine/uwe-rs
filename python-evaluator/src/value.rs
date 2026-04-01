//! Runtime values: one variant per kind of Python object the subset knows.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::any::Any;
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use indexmap::{IndexMap, IndexSet};
use num_bigint::BigInt;
use num_traits::ToPrimitive;
use ruff_python_ast::{ExprLambda, Parameters, StmtFunctionDef};

use crate::error::Error;
use crate::interpreter::{Interpreter, Source};

/// Names of variables, attributes and keyword arguments.
pub type Name = Rc<str>;

/// A Python object. Lists, dicts and sets are shared: a change through one holder is seen by
/// all, as in Python.
#[derive(Clone)]
pub enum Value {
    None,
    Bool(bool),
    Int(i64),
    /// An integer outside the range of `i64`; results that fit are always `Int`.
    BigInt(Rc<BigInt>),
    Float(f64),
    Str(Rc<str>),
    Bytes(Rc<[u8]>),
    List(Rc<RefCell<Vec<Value>>>),
    Tuple(Rc<[Value]>),
    Dict(Rc<RefCell<Dict>>),
    Set(Rc<RefCell<IndexSet<Key>>>),
    FrozenSet(Rc<IndexSet<Key>>),
    Range(Rc<Range>),
    Slice(Rc<[Value; 3]>),
    /// A function or lambda written in Python.
    Function(Rc<Function>),
    /// A function written in Rust.
    Builtin(Rc<Builtin>),
    /// A function bound to the object it was looked up on.
    Method(Rc<Method>),
    Class(Rc<Class>),
    /// An object of a class written in Python, exceptions included.
    Instance(Rc<Instance>),
    Module(Rc<Module>),
    Iterator(Rc<RefCell<Iter>>),
    /// A value with named flags, like Snakemake's `temp("a.txt")`. It behaves like the value
    /// it wraps, and keeps its flags while it is passed around unchanged.
    Tagged(Rc<Tagged>),
    /// An object that the host or a library stand-in implements in Rust.
    Native(Rc<dyn Native>),
    Ellipsis,
    NotImplemented,
}

impl std::fmt::Debug for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&crate::format::debug_repr(self))
    }
}

impl Value {
    pub fn str(text: impl Into<Rc<str>>) -> Value {
        Value::Str(text.into())
    }

    pub fn list(items: Vec<Value>) -> Value {
        Value::List(Rc::new(RefCell::new(items)))
    }

    pub fn tuple(items: Vec<Value>) -> Value {
        Value::Tuple(items.into())
    }

    pub fn dict(dict: Dict) -> Value {
        Value::Dict(Rc::new(RefCell::new(dict)))
    }

    /// A dict of text keys.
    pub fn dict_of(entries: impl IntoIterator<Item = (String, Value)>) -> Value {
        let mut dict = Dict::default();
        for (key, value) in entries {
            dict.entries.insert(Key(Value::str(key)), value);
        }
        Value::dict(dict)
    }

    pub fn set(items: IndexSet<Key>) -> Value {
        Value::Set(Rc::new(RefCell::new(items)))
    }

    pub fn iterator(items: impl IntoIterator<Item = Value>) -> Value {
        Value::Iterator(Rc::new(RefCell::new(Iter::Items(
            items.into_iter().collect(),
        ))))
    }

    pub fn native(object: impl Native + 'static) -> Value {
        Value::Native(Rc::new(object))
    }

    /// A builtin function that the host defines as a closure.
    pub fn builtin(
        name: &str,
        function: impl Fn(&mut Interpreter, Args) -> Result<Value, Error> + 'static,
    ) -> Value {
        Value::Builtin(Rc::new(Builtin {
            name: name.into(),
            function: Rc::new(function),
        }))
    }

    pub fn from_bigint(value: BigInt) -> Value {
        match value.to_i64() {
            Some(small) => Value::Int(small),
            None => Value::BigInt(Rc::new(value)),
        }
    }

    /// The value without its flags.
    pub fn plain(&self) -> &Value {
        match self {
            Value::Tagged(tagged) => tagged.value.plain(),
            other => other,
        }
    }

    pub fn is_none(&self) -> bool {
        matches!(self.plain(), Value::None)
    }

    pub fn as_str(&self) -> Option<&str> {
        match self.plain() {
            Value::Str(text) => Some(text),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self.plain() {
            Value::Int(value) => Some(*value),
            Value::Bool(value) => Some(i64::from(*value)),
            _ => None,
        }
    }

    pub fn as_native<T: 'static>(&self) -> Option<&T> {
        match self.plain() {
            Value::Native(object) => object.as_any().downcast_ref::<T>(),
            _ => None,
        }
    }

    /// The flags of a tagged value.
    pub fn tags(&self) -> Option<&IndexMap<Name, Value>> {
        match self {
            Value::Tagged(tagged) => Some(&tagged.tags),
            _ => None,
        }
    }

    pub fn tag(&self, name: &str) -> Option<&Value> {
        self.tags().and_then(|tags| tags.get(name))
    }

    /// The same value with one more flag.
    pub fn with_tag(&self, name: &str, value: Value) -> Value {
        let (inner, mut tags) = match self {
            Value::Tagged(tagged) => (tagged.value.clone(), tagged.tags.clone()),
            other => (other.clone(), IndexMap::new()),
        };
        tags.insert(name.into(), value);
        Value::Tagged(Rc::new(Tagged { value: inner, tags }))
    }

    /// Whether two values are the same object, as `is` tells.
    pub fn is(&self, other: &Value) -> bool {
        match (self.plain(), other.plain()) {
            (Value::None, Value::None) => true,
            (Value::Ellipsis, Value::Ellipsis) => true,
            (Value::NotImplemented, Value::NotImplemented) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Str(a), Value::Str(b)) => Rc::ptr_eq(a, b) || (a.len() <= 1 && a == b),
            (Value::List(a), Value::List(b)) => Rc::ptr_eq(a, b),
            (Value::Dict(a), Value::Dict(b)) => Rc::ptr_eq(a, b),
            (Value::Set(a), Value::Set(b)) => Rc::ptr_eq(a, b),
            (Value::Tuple(a), Value::Tuple(b)) => Rc::ptr_eq(a, b),
            (Value::Function(a), Value::Function(b)) => Rc::ptr_eq(a, b),
            (Value::Builtin(a), Value::Builtin(b)) => Rc::ptr_eq(a, b),
            (Value::Class(a), Value::Class(b)) => Rc::ptr_eq(a, b),
            (Value::Instance(a), Value::Instance(b)) => Rc::ptr_eq(a, b),
            (Value::Module(a), Value::Module(b)) => Rc::ptr_eq(a, b),
            (Value::Iterator(a), Value::Iterator(b)) => Rc::ptr_eq(a, b),
            (Value::Native(a), Value::Native(b)) => Rc::ptr_eq(a, b),
            _ => false,
        }
    }

    /// An address for `id()` and for hashing objects by identity.
    pub fn address(&self) -> usize {
        match self.plain() {
            Value::List(value) => Rc::as_ptr(value) as *const u8 as usize,
            Value::Dict(value) => Rc::as_ptr(value) as *const u8 as usize,
            Value::Set(value) => Rc::as_ptr(value) as *const u8 as usize,
            Value::Tuple(value) => Rc::as_ptr(value) as *const u8 as usize,
            Value::Str(value) => Rc::as_ptr(value) as *const u8 as usize,
            Value::Function(value) => Rc::as_ptr(value) as usize,
            Value::Builtin(value) => Rc::as_ptr(value) as usize,
            Value::Method(value) => Rc::as_ptr(value) as usize,
            Value::Class(value) => Rc::as_ptr(value) as usize,
            Value::Instance(value) => Rc::as_ptr(value) as usize,
            Value::Module(value) => Rc::as_ptr(value) as usize,
            Value::Iterator(value) => Rc::as_ptr(value) as *const u8 as usize,
            Value::Native(value) => Rc::as_ptr(value) as *const u8 as usize,
            Value::Int(value) => *value as usize,
            _ => 0,
        }
    }
}

/// A dict, in insertion order. `defaultdict` and `Counter` are dicts with a default.
#[derive(Clone, Default)]
pub struct Dict {
    pub entries: IndexMap<Key, Value>,
    pub kind: DictKind,
}

#[derive(Clone, Default)]
pub enum DictKind {
    #[default]
    Plain,
    OrderedDict,
    /// `collections.defaultdict` with its factory, `None` for none.
    Default(Value),
    Counter,
}

impl Dict {
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.entries.get(&Key(Value::str(key)))
    }

    pub fn insert(&mut self, key: &str, value: Value) {
        self.entries.insert(Key(Value::str(key)), value);
    }
}

/// A value that can be a dict key or a set member, compared and hashed as Python does:
/// `1`, `1.0` and `True` are the same key.
#[derive(Clone, Debug)]
pub struct Key(pub Value);

impl Key {
    /// A key, or a `TypeError` for unhashable values like lists.
    pub fn new(value: Value) -> Result<Key, Error> {
        check_hashable(&value)?;
        Ok(Key(value))
    }

    pub fn str(text: &str) -> Key {
        Key(Value::str(text))
    }
}

fn check_hashable(value: &Value) -> Result<(), Error> {
    match value.plain() {
        Value::List(_) | Value::Dict(_) | Value::Set(_) | Value::Slice(_) => {
            Err(Error::type_error(format!(
                "unhashable type: '{}'",
                crate::types::type_name(value)
            )))
        }
        Value::Tuple(items) => items.iter().try_for_each(check_hashable),
        Value::Native(object) if !object.hashable() => Err(Error::type_error(format!(
            "unhashable type: '{}'",
            object.type_name()
        ))),
        _ => Ok(()),
    }
}

impl PartialEq for Key {
    fn eq(&self, other: &Self) -> bool {
        key_equal(&self.0, &other.0)
    }
}

impl Eq for Key {}

impl Hash for Key {
    fn hash<H: Hasher>(&self, state: &mut H) {
        key_hash(&self.0, state);
    }
}

/// Equality of hashable values without calling Python code.
pub fn key_equal(a: &Value, b: &Value) -> bool {
    use Value::*;
    match (a.plain(), b.plain()) {
        (None, None) | (Ellipsis, Ellipsis) => true,
        (Str(a), Str(b)) => a == b,
        (Bytes(a), Bytes(b)) => a == b,
        (Tuple(a), Tuple(b)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(a, b)| key_equal(a, b))
        }
        (FrozenSet(a), FrozenSet(b)) => a == b,
        (
            a @ (Bool(_) | Int(_) | BigInt(_) | Float(_)),
            b @ (Bool(_) | Int(_) | BigInt(_) | Float(_)),
        ) => crate::number::numbers_equal(a, b),
        (Class(a), Class(b)) => Rc::ptr_eq(a, b),
        (Native(a), Native(b)) => Rc::ptr_eq(a, b) || a.key_equal(b.as_ref()),
        (a, b) => a.is(b),
    }
}

fn key_hash<H: Hasher>(value: &Value, state: &mut H) {
    match value.plain() {
        Value::None => 0u8.hash(state),
        Value::Bool(value) => i64::from(*value).hash(state),
        Value::Int(value) => value.hash(state),
        Value::BigInt(value) => value.hash(state),
        Value::Float(value) => {
            if value.fract() == 0.0 && value.abs() < 9.2e18 {
                (*value as i64).hash(state);
            } else {
                value.to_bits().hash(state);
            }
        }
        Value::Str(text) => text.hash(state),
        Value::Bytes(bytes) => bytes.hash(state),
        Value::Tuple(items) => {
            items.len().hash(state);
            for item in items.iter() {
                key_hash(item, state);
            }
        }
        Value::FrozenSet(items) => items.len().hash(state),
        Value::Native(object) => object.key_hash(state),
        other => other.address().hash(state),
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Range {
    pub start: i64,
    pub stop: i64,
    pub step: i64,
}

impl Range {
    pub fn len(&self) -> usize {
        let Range { start, stop, step } = *self;
        let count = if step > 0 && start < stop {
            (stop - start - 1) / step + 1
        } else if step < 0 && start > stop {
            (start - stop - 1) / (-step) + 1
        } else {
            0
        };
        count as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get(&self, index: usize) -> i64 {
        self.start + self.step * index as i64
    }
}

/// A function or lambda defined in Python code.
pub struct Function {
    pub name: Name,
    pub qualname: Name,
    pub code: Code,
    /// The values of the parameter defaults, by parameter name.
    pub defaults: HashMap<Name, Value>,
    pub globals: Rc<Scope>,
    /// The scope of the enclosing function, for closures.
    pub closure: Option<Rc<Scope>>,
    pub source: Rc<Source>,
    /// Whether the body holds `yield`; calls then collect what it yields.
    pub generator: bool,
    /// Names declared `global` or `nonlocal` in the body.
    pub declared: Rc<Declared>,
    /// Attributes set on the function, like `__name__` or cache state.
    pub attributes: RefCell<IndexMap<Name, Value>>,
}

#[derive(Default, Debug)]
pub struct Declared {
    pub global: Vec<Name>,
    pub nonlocal: Vec<Name>,
}

/// The syntax of a function.
#[derive(Clone)]
pub enum Code {
    Def(Rc<StmtFunctionDef>),
    Lambda(Rc<ExprLambda>),
}

impl Code {
    pub fn parameters(&self) -> Option<&Parameters> {
        match self {
            Code::Def(def) => Some(&def.parameters),
            Code::Lambda(lambda) => lambda.parameters.as_deref(),
        }
    }

    /// Names of the parameters in order, including `*args` and `**kwargs`.
    pub fn parameter_names(&self) -> Vec<String> {
        let Some(parameters) = self.parameters() else {
            return Vec::new();
        };
        let mut names: Vec<String> = parameters
            .posonlyargs
            .iter()
            .chain(parameters.args.iter())
            .map(|parameter| parameter.parameter.name.id.to_string())
            .collect();
        if let Some(vararg) = &parameters.vararg {
            names.push(vararg.name.id.to_string());
        }
        names.extend(
            parameters
                .kwonlyargs
                .iter()
                .map(|parameter| parameter.parameter.name.id.to_string()),
        );
        if let Some(kwarg) = &parameters.kwarg {
            names.push(kwarg.name.id.to_string());
        }
        names
    }
}

pub type NativeFunction = Rc<dyn Fn(&mut Interpreter, Args) -> Result<Value, Error>>;

/// A function implemented in Rust.
pub struct Builtin {
    pub name: Name,
    pub function: NativeFunction,
}

/// A callable bound to the object it came from.
pub struct Method {
    pub receiver: Value,
    pub function: MethodFunction,
}
