//! Runtime values, one variant per Java class a Nextflow script can see.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod class;
mod compare;
mod format;
mod key;
mod object;
mod operators;
mod path;
mod range;

use std::cell::RefCell;
use std::rc::Rc;

use indexmap::{IndexMap, IndexSet};

use crate::exception::Exception;
use crate::lowered::{Code, Module};
use crate::number::Number;

pub use class::{class_text, qualified_class_name, simple_class_name};
pub use compare::Relation;
pub use key::Key;
pub use object::{JsonOptions, Object, State};
pub use operators::Operator;
pub use path::PathValue;
pub use range::{Range, RangeClass};

/// A value of a running script. Lists, maps and sets are shared: a change through one holder
/// is visible to all, as in Groovy.
#[derive(Clone, Debug)]
pub enum Value {
    Null,
    Boolean(bool),
    Number(Number),
    String(Rc<str>),
    /// A GString written inside the `${...}` of another GString, which Nextflow does not turn
    /// into a `String`. It prints like its text, but is never Java-equal to a `String`.
    GString(Rc<str>),
    List(Rc<RefCell<List>>),
    Map(Rc<RefCell<Map>>),
    Set(Rc<RefCell<Set>>),
    /// A `Map.Entry`, as `find` and `each` on a map give it.
    MapEntry(Rc<(Value, Value)>),
    /// `nextflow.util.MemoryUnit`: a byte count that is never negative.
    MemoryUnit(i64),
    /// `nextflow.util.Duration`: a millisecond count that is never negative.
    Duration(i64),
    /// `java.util.Date`: milliseconds since 1970, shown in the local time zone.
    Date(i64),
    Closure(Rc<Closure>),
    /// A thrown and caught exception, as `catch (e)` binds it.
    Exception(Rc<Exception>),
    /// `java.lang.Class`, by the fully qualified name of the class.
    Class(Rc<str>),
    /// An object of a Java class that iunctus stands in for.
    Object(Rc<RefCell<Object>>),
    /// `java.util.regex.Pattern`, from `~'text'`.
    Pattern(Rc<crate::regex::Pattern>),
    /// `java.util.regex.Matcher`, from `text =~ pattern`.
    Matcher(Rc<crate::regex::Matcher>),
    /// `java.nio.file.Path`, from `file()` and path operations.
    Path(Rc<PathValue>),
    /// A channel of the dataflow network, by its number in the network.
    Channel(ChannelId),
    /// The outputs of a process or workflow call.
    ChannelOut(Rc<ChannelOut>),
    /// A process or workflow of a script used as a value, as in `FASTQC.out`.
    Definition(Rc<DefinitionRef>),
    /// A namespace whose methods the host provides, like `channel` or `log`.
    Namespace(Rc<str>),
    /// An operator called without a receiver, applied to the channel piped into it:
    /// `ch | map { ... }`.
    Operator(Rc<(Rc<str>, Vec<Value>)>),
}

/// The number of a channel in the dataflow network.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ChannelId(pub u32);

/// The outputs of a process or workflow call, by position and by `emit` name.
#[derive(Debug)]
pub struct ChannelOut {
    pub values: Vec<Value>,
    pub names: Vec<Option<Rc<str>>>,
}

/// A process or workflow of a script.
#[derive(Debug)]
pub struct DefinitionRef {
    pub module: Rc<Module>,
    pub name: Rc<str>,
}

/// A closure: its code, the script it belongs to, and the variables it captured.
pub struct Closure {
    pub code: Rc<Code>,
    pub module: Rc<Module>,
    pub captured: Vec<Rc<RefCell<Value>>>,
}

impl std::fmt::Debug for Closure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Closure({})", self.code.name)
    }
}

#[derive(Clone, Debug)]
pub struct List {
    pub kind: ListKind,
    pub items: Vec<Value>,
    /// The bounds of a range, whose elements are in `items`.
    pub range: Option<Rc<Range>>,
}

impl List {
    pub fn new(kind: ListKind, items: Vec<Value>) -> Self {
        Self {
            kind,
            items,
            range: None,
        }
    }
}

/// The Java classes behind a list; they differ in class name, equality and what they allow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListKind {
    /// `java.util.ArrayList`, from list literals and most methods.
    ArrayList,
    /// `nextflow.util.ArrayTuple`, which rejects adding, removing and setting elements.
    Tuple,
    /// `java.util.LinkedList`, from `file()` with a glob.
    LinkedList,
    /// `String[]`, from `split`. Java arrays are only equal to themselves.
    Array,
    /// A range, like `1..3`, which cannot change.
    Range,
    /// `nextflow.util.BlankSeparatedList`: the files of one task input, which Java's
    /// `toString()` joins with spaces.
    BlankSeparated,
}

#[derive(Clone, Debug)]
pub struct Map {
    pub kind: MapKind,
    pub entries: IndexMap<Key, Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapKind {
    /// `java.util.LinkedHashMap`, from map literals; keeps insertion order.
    LinkedHashMap,
    /// `nextflow.util.RecordMap`, which rejects `put`, `remove` and `clear`.
    Record,
    /// `java.util.TreeMap`, from `sort()`; keeps its keys sorted.
    TreeMap,
    /// `nextflow.processor.LazyMap`, the `task.ext` map: reading a closure value calls it
    /// with the task's variables in scope.
    Lazy,
}

#[derive(Clone, Debug)]
pub struct Set {
    pub kind: SetKind,
    pub items: IndexSet<Key>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetKind {
    /// `java.util.LinkedHashSet`, from `as Set`; keeps insertion order.
    LinkedHashSet,
    /// `java.util.HashSet`, from `toSet()`; iterates in Java's hash table order.
    HashSet,
}

impl Value {
    pub fn string(text: impl Into<Rc<str>>) -> Value {
        Value::String(text.into())
    }

    pub fn integer(value: i32) -> Value {
        Value::Number(Number::Integer(value))
    }

    /// A new `ArrayList`.
    pub fn list(items: Vec<Value>) -> Value {
        Value::List(Rc::new(RefCell::new(List::new(ListKind::ArrayList, items))))
    }

    /// A new `LinkedHashMap`; a later entry with an equal key replaces the value in place.
    pub fn map(entries: impl IntoIterator<Item = (Value, Value)>) -> Value {
        let entries = entries
            .into_iter()
            .map(|(key, value)| (Key(key), value))
            .collect();
        Value::Map(Rc::new(RefCell::new(Map {
            kind: MapKind::LinkedHashMap,
            entries,
        })))
    }

    /// A new `LinkedHashSet`.
    pub fn set(items: impl IntoIterator<Item = Value>) -> Value {
        Value::Set(Rc::new(RefCell::new(Set {
            kind: SetKind::LinkedHashSet,
            items: items.into_iter().map(Key).collect(),
        })))
    }

    /// The simple name of the Java class, as `getClass().getSimpleName()` gives it.
    pub fn class_name(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Boolean(_) => "Boolean",
            Value::Number(number) => number.class_name(),
            Value::String(_) => "String",
            Value::GString(_) => "GStringImpl",
            Value::List(list) => match list.borrow().kind {
                ListKind::ArrayList => "ArrayList",
                ListKind::Tuple => "ArrayTuple",
                ListKind::LinkedList => "LinkedList",
                ListKind::Array => "String[]",
                ListKind::BlankSeparated => "BlankSeparatedList",
                ListKind::Range => list
                    .borrow()
                    .range
                    .as_ref()
                    .map_or("IntRange", |r| r.class.name()),
            },
            Value::Map(map) => match map.borrow().kind {
                MapKind::LinkedHashMap => "LinkedHashMap",
                MapKind::Record => "RecordMap",
                MapKind::TreeMap => "TreeMap",
                MapKind::Lazy => "LazyMap",
            },
            Value::Set(set) => match set.borrow().kind {
                SetKind::LinkedHashSet => "LinkedHashSet",
                SetKind::HashSet => "HashSet",
            },
            Value::MapEntry(_) => "Entry",
            Value::MemoryUnit(_) => "MemoryUnit",
            Value::Duration(_) => "Duration",
            Value::Date(_) => "Date",
            Value::Closure(_) => "Closure",
            Value::Exception(exception) => exception.class,
            Value::Class(_) => "Class",
            Value::Object(object) => super::value::simple_class_name(object.borrow().class),
            Value::Pattern(_) => "Pattern",
            Value::Matcher(_) => "Matcher",
            Value::Path(path) => match path.scheme() {
                "file" => "UnixPath",
                "s3" => "S3Path",
                _ => "XPath",
            },
            Value::Channel(_) => "DataflowBroadcast",
            Value::ChannelOut(_) => "ChannelOut",
            Value::Definition(_) => "ProcessDef",
            Value::Namespace(_) => "Namespace",
            Value::Operator(_) => "OpCall",
        }
    }

    /// The fully qualified Java class name, as error messages show it.
    pub fn java_class_name(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Boolean(_) => "java.lang.Boolean",
            Value::Number(number) => number.java_class(),
            Value::String(_) => "java.lang.String",
            Value::GString(_) => "org.codehaus.groovy.runtime.GStringImpl",
            Value::List(list) => match list.borrow().kind {
                ListKind::ArrayList => "java.util.ArrayList",
                ListKind::Tuple => "nextflow.util.ArrayTuple",
                ListKind::LinkedList => "java.util.LinkedList",
                ListKind::Array => "[Ljava.lang.String;",
                ListKind::BlankSeparated => "nextflow.util.BlankSeparatedList",
                ListKind::Range => match list.borrow().range.as_ref().map(|range| range.class) {
                    Some(RangeClass::Number) => "groovy.lang.NumberRange",
                    Some(RangeClass::Object) => "groovy.lang.ObjectRange",
                    Some(RangeClass::Empty) => "groovy.lang.EmptyRange",
                    _ => "groovy.lang.IntRange",
                },
            },
            Value::Map(map) => match map.borrow().kind {
                MapKind::LinkedHashMap => "java.util.LinkedHashMap",
                MapKind::Record => "nextflow.util.RecordMap",
                MapKind::TreeMap => "java.util.TreeMap",
                MapKind::Lazy => "nextflow.processor.LazyMap",
            },
            Value::Set(set) => match set.borrow().kind {
                SetKind::LinkedHashSet => "java.util.LinkedHashSet",
                SetKind::HashSet => "java.util.HashSet",
            },
            Value::MapEntry(_) => "java.util.LinkedHashMap$Entry",
            Value::MemoryUnit(_) => "nextflow.util.MemoryUnit",
            Value::Duration(_) => "nextflow.util.Duration",
            Value::Date(_) => "java.util.Date",
            Value::Closure(_) => "groovy.lang.Closure",
            Value::Exception(exception) => exception.qualified_class(),
            Value::Class(_) => "java.lang.Class",
            Value::Object(object) => object.borrow().class,
            Value::Pattern(_) => "java.util.regex.Pattern",
            Value::Matcher(_) => "java.util.regex.Matcher",
            Value::Path(path) => match path.scheme() {
                "file" => "sun.nio.fs.UnixPath",
                "s3" => "nextflow.cloud.aws.nio.S3Path",
                _ => "nextflow.file.http.XPath",
            },
            Value::Channel(_) => "groovyx.gpars.dataflow.DataflowBroadcast",
            Value::ChannelOut(_) => "nextflow.script.ChannelOut",
            Value::Definition(_) => "nextflow.script.ProcessDef",
            Value::Namespace(_) => "nextflow.Channel",
            Value::Operator(_) => "nextflow.extension.OpCall",
        }
    }

    /// Groovy truth, as `if`, `!`, `&&`, `||` and `?:` see the value.
    pub fn truth(&self) -> bool {
        match self {
            Value::Null => false,
            Value::Boolean(value) => *value,
            Value::Number(number) => {
                let value = number.to_f64();
                value != 0.0 && !value.is_nan()
            }
            Value::String(text) | Value::GString(text) => !text.is_empty(),
            Value::List(list) => !list.borrow().items.is_empty(),
            Value::Map(map) => !map.borrow().entries.is_empty(),
            Value::Set(set) => !set.borrow().items.is_empty(),
            Value::MapEntry(_) => true,
            Value::MemoryUnit(size) => *size != 0,
            Value::Duration(millis) => *millis != 0,
            Value::Date(_)
            | Value::Class(_)
            | Value::Object(_)
            | Value::Closure(_)
            | Value::Exception(_)
            | Value::Pattern(_)
            | Value::Path(_) => true,
            Value::Channel(_)
            | Value::ChannelOut(_)
            | Value::Definition(_)
            | Value::Namespace(_)
            | Value::Operator(_) => true,
            // Groovy's `Matcher.asBoolean()` is `find(0)`: the pattern occurs somewhere.
            Value::Matcher(matcher) => !matcher.matches.is_empty(),
        }
    }
}
