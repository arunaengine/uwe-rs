//! The values of a job as plain data, which each language's preamble writes in its own
//! syntax. Snakemake hands the values to a script by pickling them (Python) or by writing them
//! as code (R, Julia, Bash, Rust); iunctus writes code for every language, so it first turns
//! the interpreter's values into this plain form.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use python_evaluator::format::quote;
use python_evaluator::number::float_repr;
use python_evaluator::pandas::{Array, DataFrame, Series};
use python_evaluator::{Args, Interpreter, Value};

use crate::objects::NamedList;
use crate::rule::Items;

/// A value of a job in plain form.
#[derive(Clone, Debug, PartialEq)]
pub enum Data {
    None,
    Bool(bool),
    /// An integer by its decimal digits, so that big ones stay exact.
    Int(String),
    Float(f64),
    Complex(f64, f64),
    Str(String),
    Bytes(Vec<u8>),
    /// A `pathlib` path.
    Path(String),
    List(Vec<Data>),
    Tuple(Vec<Data>),
    Set(Vec<Data>),
    Dict(Vec<(Data, Data)>),
    Named(Named),
    /// A pandas, numpy or polars object, which Snakemake stores as plain data and makes again
    /// in the script: its kind (like `pd.DataFrame`), the stored data, and the items that
    /// iterating it gives (the column names of a table).
    Table {
        kind: &'static str,
        stored: Box<Data>,
        items: Vec<Data>,
    },
    /// A numpy scalar of a type, like `float64`, as `Paramspace.instance()` gives numbers.
    Scalar {
        kind: String,
        value: Box<Data>,
    },
    /// Any other object, like a function: no language can take it. Its `repr()` names it, and
    /// its `str()` is what Bash gets, as Snakemake gives Bash `str()` of every value.
    Opaque {
        repr: String,
        text: String,
    },
}

/// Snakemake's `Namedlist`: items, some of them or ranges of them with names.
#[derive(Clone, Debug, PartialEq)]
pub struct Named {
    /// The class, like `InputFiles` or `Params`.
    pub kind: String,
    pub values: Vec<Data>,
    /// A name, its first item and, for a name of several items, the end of their range.
    pub names: Vec<(String, usize, Option<usize>)>,
}

impl Named {
    pub fn new(kind: &str, values: Vec<Data>) -> Named {
        Named {
            kind: kind.to_owned(),
            values,
            names: Vec::new(),
        }
    }

    /// The value of a name: one item, or a `Namedlist` of the items of a range.
    pub fn named(&self, name: &str) -> Option<Data> {
        let (_, start, end) = self
            .names
            .iter()
            .find(|(candidate, _, _)| candidate == name)?;
        Some(match end {
            None => self.values.get(*start).cloned().unwrap_or(Data::None),
            Some(end) => {
                let end = (*end).min(self.values.len());
                let values = self.values[(*start).min(end)..end].to_vec();
                Data::Named(Named::new("Namedlist", values))
            }
        })
    }

    /// `items()`: each name with its value, in the order the names were given.
    pub fn items(&self) -> Vec<(String, Data)> {
        self.names
            .iter()
            .map(|(name, _, _)| (name.clone(), self.named(name).unwrap_or(Data::None)))
            .collect()
    }

    /// `_allitems()`: the items in order, a name for each named item or range and `None` for
    /// the others.
    pub fn all_items(&self) -> Vec<(Option<String>, Data)> {
        let mut names = self.names.clone();
        names.sort_by_key(|(_, start, end)| (*start, end.unwrap_or(*start + 1)));
        let mut items = Vec::new();
        let mut next = 0;
        for (name, start, end) in names {
            let end = end.unwrap_or(start + 1);
            if start > next {
                for value in self.values.iter().take(start).skip(next) {
                    items.push((None, value.clone()));
                }
            }
            let value = self.named(&name).unwrap_or(Data::None);
            items.push((Some(name), value));
            next = end;
        }
        for value in self.values.iter().skip(next) {
            items.push((None, value.clone()));
        }
        items
    }
}

impl Data {
    pub fn str(text: impl Into<String>) -> Data {
        Data::Str(text.into())
    }

    pub fn int(value: i64) -> Data {
        Data::Int(value.to_string())
    }

    /// Whether Python counts it as a number, as `numbers.Number` does: booleans too.
    pub fn is_number(&self) -> bool {
        match self {
            Data::Bool(_) | Data::Int(_) | Data::Float(_) | Data::Complex(..) => true,
            // numpy's booleans are no numbers.
            Data::Scalar { kind, .. } => kind != "bool_",
            _ => false,
        }
    }

    /// The items that iterating the value gives, for values that Python can iterate, except
    /// texts: a dict gives its keys.
    pub fn items(&self) -> Option<Vec<Data>> {
        match self {
            Data::List(items) | Data::Tuple(items) | Data::Set(items) => Some(items.clone()),
            Data::Dict(entries) => Some(entries.iter().map(|(key, _)| key.clone()).collect()),
            Data::Named(named) => Some(named.values.clone()),
            Data::Bytes(bytes) => Some(
                bytes
                    .iter()
                    .map(|byte| Data::int(i64::from(*byte)))
                    .collect(),
            ),
            Data::Table { items, .. } => Some(items.clone()),
            _ => None,
        }
    }

    /// Python's `repr()`.
    pub fn repr(&self) -> String {
        match self {
            Data::None => "None".into(),
            Data::Bool(true) => "True".into(),
            Data::Bool(false) => "False".into(),
            Data::Int(digits) => digits.clone(),
            Data::Float(value) => float_repr(*value),
            Data::Complex(real, imag) => complex_repr(*real, *imag),
            Data::Str(text) => quote(text),
            Data::Bytes(bytes) => python_evaluator::format::bytes_repr(bytes),
            Data::Path(path) => format!("PosixPath({})", quote(path)),
            Data::List(items) => format!("[{}]", joined(items, Data::repr)),
            Data::Tuple(items) if items.len() == 1 => format!("({},)", items[0].repr()),
            Data::Tuple(items) => format!("({})", joined(items, Data::repr)),
            Data::Set(items) if items.is_empty() => "set()".into(),
            Data::Set(items) => format!("{{{}}}", joined(items, Data::repr)),
            Data::Dict(entries) => format!(
                "{{{}}}",
                entries
                    .iter()
                    .map(|(key, value)| format!("{}: {}", key.repr(), value.repr()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Data::Named(named) => format!("[{}]", joined(&named.values, Data::repr)),
            Data::Table { stored, .. } => stored.repr(),
            Data::Scalar { kind, value } => format!("np.{kind}({})", value.repr()),
            Data::Opaque { repr, .. } => repr.clone(),
        }
    }

    /// Python's `str()`.
    pub fn text(&self) -> String {
        match self {
            Data::Str(text) | Data::Path(text) => text.clone(),
            Data::Named(named) => named
                .values
                .iter()
                .map(Data::text)
                .collect::<Vec<_>>()
                .join(" "),
            Data::Opaque { text, .. } => text.clone(),
            Data::Scalar { value, .. } => value.text(),
            other => other.repr(),
        }
    }
}

fn joined(items: &[Data], show: fn(&Data) -> String) -> String {
    items.iter().map(show).collect::<Vec<_>>().join(", ")
}

/// Python's `repr()` of a complex number, like `(1+2j)` or `2j`.
fn complex_repr(real: f64, imag: f64) -> String {
    let part = |value: f64| {
        let text = float_repr(value);
        text.strip_suffix(".0").map(str::to_owned).unwrap_or(text)
    };
    if real == 0.0 && real.is_sign_positive() {
        return format!("{}j", part(imag));
    }
    let sign = if imag < 0.0 || (imag == 0.0 && imag.is_sign_negative()) {
        "-"
    } else {
        "+"
    };
    format!("({}{sign}{}j)", part(real), part(imag.abs()))
}

/// The plain form of the items of a rule directive with their names, as texts, the way
/// `Namedlist._plainstrings()` gives files to scripts.
pub fn files(kind: &str, items: &Items) -> Data {
    let values = items
        .values
        .iter()
        .map(|value| match value.as_str() {
            Some(text) => Data::str(text),
            None => Data::str(python_evaluator::format::plain_str(value)),
        })
        .collect();
    Data::Named(Named {
        kind: kind.to_owned(),
        values,
        names: items.names.clone(),
    })
}

/// The plain form of a value of the interpreter.
pub fn from_value(interpreter: &mut Interpreter, value: &Value) -> Data {
    if let Some(kind) = value
        .tag(crate::paramspace::NUMPY_TAG)
        .and_then(Value::as_str)
    {
        return Data::Scalar {
            kind: kind.to_owned(),
            value: Box::new(from_value(interpreter, value.plain())),
        };
    }
    match value.plain() {
        Value::None => Data::None,
        Value::Bool(flag) => Data::Bool(*flag),
        Value::Int(number) => Data::int(*number),
        Value::BigInt(number) => Data::Int(number.to_string()),
        Value::Float(number) => Data::Float(*number),
        Value::Str(text) => Data::str(&**text),
        Value::Bytes(bytes) => Data::Bytes(bytes.to_vec()),
        Value::List(items) => {
            let items = items.borrow().clone();
            Data::List(all(interpreter, &items))
        }
        Value::Tuple(items) => Data::Tuple(all(interpreter, items)),
        Value::Set(items) => {
            let items: Vec<Value> = items.borrow().iter().map(|key| key.0.clone()).collect();
            Data::Set(all(interpreter, &items))
        }
        Value::FrozenSet(items) => {
            let items: Vec<Value> = items.iter().map(|key| key.0.clone()).collect();
            Data::Set(all(interpreter, &items))
        }
        Value::Dict(dict) => {
            let entries: Vec<(Value, Value)> = dict
                .borrow()
                .entries
                .iter()
                .map(|(key, value)| (key.0.clone(), value.clone()))
                .collect();
            Data::Dict(
                entries
                    .iter()
                    .map(|(key, value)| {
                        (from_value(interpreter, key), from_value(interpreter, value))
                    })
                    .collect(),
            )
        }
        Value::Range(range) => {
            let items: Vec<Data> = (0..range.len())
                .map(|index| Data::int(range.get(index)))
                .collect();
            Data::List(items)
        }
        Value::Native(object) => native(interpreter, value, object.type_name()),
        _ => opaque(interpreter, value),
    }
}

fn all(interpreter: &mut Interpreter, values: &[Value]) -> Vec<Data> {
    values
        .iter()
        .map(|value| from_value(interpreter, value))
        .collect()
}

/// Objects of the host and of the library stand-ins that have a plain form.
fn native(interpreter: &mut Interpreter, value: &Value, type_name: &str) -> Data {
    if let Some(list) = value.as_native::<NamedList>() {
        return Data::Named(Named {
            kind: list.kind.to_owned(),
            values: all(interpreter, &list.values),
            names: list.names.clone(),
        });
    }
    if let Some(complex) = value.as_native::<python_evaluator::complex::Complex>() {
        return Data::Complex(complex.0, complex.1);
    }
    if matches!(type_name, "PosixPath" | "Path" | "PurePosixPath") {
        return match interpreter.to_str(value) {
            Ok(text) => Data::Path(text),
            Err(_) => opaque(interpreter, value),
        };
    }
    // Snakemake stores tables as dicts and arrays as lists, and makes them again in the
    // script.
    let stored = if value.as_native::<DataFrame>().is_some() {
        Some(("pd.DataFrame", "to_dict"))
    } else if value.as_native::<Series>().is_some() {
        Some(("pd.Series", "to_dict"))
    } else if value.as_native::<Array>().is_some() {
        Some(("np.ndarray", "tolist"))
    } else {
        None
    };
    if let Some((kind, method)) = stored {
        let converted = interpreter
            .get_attribute(value, method)
            .and_then(|method| interpreter.call(&method, Args::default()));
        let iterated = interpreter.iterate(value);
        if let (Ok(converted), Ok(iterated)) = (converted, iterated) {
            return Data::Table {
                kind,
                stored: Box::new(from_value(interpreter, &converted)),
                items: all(interpreter, &iterated),
            };
        }
    }
    opaque(interpreter, value)
}

fn opaque(interpreter: &mut Interpreter, value: &Value) -> Data {
    let repr = interpreter
        .repr(value)
        .unwrap_or_else(|_| python_evaluator::format::plain_repr(value));
    let text = interpreter.to_str(value).unwrap_or_else(|_| repr.clone());
    Data::Opaque { repr, text }
}

#[cfg(test)]
mod tests {
    use super::{Data, Named, complex_repr};

    #[test]
    fn formats_complex_numbers() {
        assert_eq!(complex_repr(1.0, 2.0), "(1+2j)");
        assert_eq!(complex_repr(0.0, 2.0), "2j");
        assert_eq!(complex_repr(1.5, -2.0), "(1.5-2j)");
    }

    #[test]
    fn serializes_named_lists() {
        let named = Named {
            kind: "InputFiles".into(),
            values: vec![Data::str("a"), Data::str("b"), Data::str("c")],
            names: vec![("second".into(), 1, None), ("rest".into(), 1, Some(3))],
        };
        assert_eq!(named.named("second"), Some(Data::str("b")));
        let items = named.all_items();
        assert_eq!(items[0], (None, Data::str("a")));
        assert_eq!(items[1].0.as_deref(), Some("second"));
        assert_eq!(items[2].0.as_deref(), Some("rest"));
        assert_eq!(items.len(), 3);
    }
}
