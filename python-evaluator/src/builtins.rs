//! The builtin functions and the constructors of the builtin types.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::any::Any;
use std::cell::RefCell;
use std::cmp::Ordering;
use std::rc::Rc;

use indexmap::{IndexMap, IndexSet};

use crate::error::Error;
use crate::interpreter::{Interpreter, bind_method};
use crate::number::{self, to_f64};
use crate::types::{self, TypeTag};
use crate::value::{Args, Class, Dict, DictKind, Function, Key, Native, Range, Scope, Value};

/// A builtin function without state.
type BuiltinFunction = fn(&mut Interpreter, Args) -> Result<Value, Error>;

/// Fills the builtins scope of a new interpreter.
pub fn install(interpreter: &mut Interpreter) {
    let builtins = interpreter.builtins.clone();
    for (tag, name) in [
        (TypeTag::Object, "object"),
        (TypeTag::Type, "type"),
        (TypeTag::Int, "int"),
        (TypeTag::Float, "float"),
        (TypeTag::Bool, "bool"),
        (TypeTag::Str, "str"),
        (TypeTag::Bytes, "bytes"),
        (TypeTag::List, "list"),
        (TypeTag::Tuple, "tuple"),
        (TypeTag::Dict, "dict"),
        (TypeTag::Set, "set"),
        (TypeTag::FrozenSet, "frozenset"),
        (TypeTag::Range, "range"),
        (TypeTag::Slice, "slice"),
        (TypeTag::Complex, "complex"),
    ] {
        builtins.set(name, Value::Class(types::builtin_class(tag)));
    }
    builtins.set(
        "bytearray",
        Value::Class(types::builtin_class(TypeTag::Bytes)),
    );
    for name in types::exception_names() {
        if let Some(class) = types::exception_class(name) {
            builtins.set(name, Value::Class(class));
        }
    }
    builtins.set("IOError", builtins.get("OSError").unwrap_or(Value::None));
    builtins.set(
        "EnvironmentError",
        builtins.get("OSError").unwrap_or(Value::None),
    );
    builtins.set("NotImplemented", Value::NotImplemented);
    builtins.set("Ellipsis", Value::Ellipsis);
    let functions: &[(&str, BuiltinFunction)] = &[
        ("abs", abs),
        ("all", all),
        ("any", any),
        ("ascii", repr),
        ("bin", |i, a| radix(i, a, 2, "0b")),
        ("callable", callable),
        ("chr", chr),
        ("classmethod", |_, a| wrap(a, "classmethod")),
        ("delattr", delattr),
        ("divmod", divmod),
        ("enumerate", enumerate),
        ("eval", |_, _| {
            Err(Error::unsupported("eval() of code text"))
        }),
        ("exec", |_, _| {
            Err(Error::unsupported("exec() of code text"))
        }),
        ("compile", |_, _| {
            Err(Error::unsupported("compile() of code text"))
        }),
        ("exit", exit),
        ("quit", exit),
        ("filter", filter),
        ("format", format),
        ("getattr", getattr),
        ("hasattr", hasattr),
        ("hash", hash),
        ("hex", |i, a| radix(i, a, 16, "0x")),
        ("id", id),
        ("input", |_, _| Err(Error::unsupported("input()"))),
        ("isinstance", isinstance),
        ("issubclass", issubclass),
        ("iter", iter),
        ("len", len),
        ("map", map),
        ("max", |i, a| extreme(i, a, Ordering::Greater, "max")),
        ("min", |i, a| extreme(i, a, Ordering::Less, "min")),
        ("next", next),
        ("oct", |i, a| radix(i, a, 8, "0o")),
        ("open", open),
        ("ord", ord),
        ("pow", pow),
        ("print", print),
        ("property", |_, a| wrap(a, "property")),
        ("repr", repr),
        ("reversed", reversed),
        ("round", round),
        ("setattr", setattr),
        ("sorted", sorted),
        ("staticmethod", |_, a| wrap(a, "staticmethod")),
        ("sum", sum),
        ("vars", vars),
        ("zip", zip),
        ("dir", dir),
        ("locals", |_, _| Ok(Value::dict(Dict::default()))),
        ("globals", |_, _| Ok(Value::dict(Dict::default()))),
        ("super", super_),
        ("__import__", import),
        ("breakpoint", |_, _| Ok(Value::None)),
    ];
    for (name, function) in functions {
        builtins.set(name, Value::builtin(name, *function));
    }
}

fn one(args: &Args, name: &str) -> Result<Value, Error> {
    args.expect(name, 1, 1)?;
    Ok(args.positional[0].clone())
}

/// Calls a builtin type: `int("3")`, `list(x)`, `dict(a=1)` and so on.
pub fn construct(
    interpreter: &mut Interpreter,
    tag: TypeTag,
    mut args: Args,
) -> Result<Value, Error> {
    let first = args.positional.first().cloned();
    match tag {
        TypeTag::Object => Ok(Value::Instance(Rc::new(crate::value::Instance {
            class: types::builtin_class(TypeTag::Object),
            dict: RefCell::new(IndexMap::new()),
        }))),
        TypeTag::Type => match args.positional.len() {
            1 => Ok(Value::Class(types::class_of(&args.positional[0]))),
            3 => {
                let name = args.positional[0].as_str().unwrap_or("type").to_owned();
                let bases: Vec<Rc<Class>> = interpreter
                    .iterate(&args.positional[1])?
                    .into_iter()
                    .filter_map(|base| match base {
                        Value::Class(class) => Some(class),
                        _ => None,
                    })
                    .collect();
                let bases = if bases.is_empty() {
                    vec![types::builtin_class(TypeTag::Object)]
                } else {
                    bases
                };
                let class = types::new_class(&name, "__main__", bases, None);
                for (key, value) in interpreter.mapping_items(&args.positional[2])? {
                    if let Some(key) = key.as_str() {
                        class.dict.borrow_mut().insert(key.into(), value);
                    }
                }
                Ok(Value::Class(class))
            }
            _ => Err(Error::type_error("type() takes 1 or 3 arguments")),
        },
        TypeTag::Bool => match first {
            Some(value) => Ok(Value::Bool(interpreter.truthy(&value)?)),
            None => Ok(Value::Bool(false)),
        },
        TypeTag::Int => {
            let base = args.take(1, "base");
            match (first, base) {
                (None, _) => Ok(Value::Int(0)),
                (Some(value), Some(base)) => {
                    let text = value
                        .as_str()
                        .ok_or_else(|| {
                            Error::type_error("int() can't convert non-string with explicit base")
                        })?
                        .to_owned();
                    number::parse_int(&text, base.as_int().unwrap_or(10) as u32)
                }
                (Some(value), None) => to_int(interpreter, &value),
            }
        }
        TypeTag::Float => match first {
            None => Ok(Value::Float(0.0)),
            Some(value) => match value.plain() {
                Value::Str(text) => Ok(Value::Float(number::parse_float(text)?)),
                Value::Bytes(bytes) => Ok(Value::Float(number::parse_float(
                    &String::from_utf8_lossy(bytes),
                )?)),
                other => match to_f64(other) {
                    Some(value) => Ok(Value::Float(value)),
                    None => {
                        if let Some(method) = interpreter.lookup_special(other, "__float__") {
                            return interpreter.call(&method, Args::default());
                        }
                        if let Value::Native(object) = other
                            && let Some(result) = object.unary(interpreter, "float")?
                        {
                            return Ok(result);
                        }
                        Err(Error::type_error(format!(
                            "float() argument must be a string or a real number, not '{}'",
                            types::type_name(other)
                        )))
                    }
                },
            },
        },
        TypeTag::Complex => Err(Error::unsupported("complex numbers")),
        TypeTag::Str => match first {
            None => Ok(Value::str("")),
            Some(value) => {
                let decode = args.positional.len() > 1 || args.keyword("encoding").is_some();
                if let Value::Bytes(bytes) = value.plain()
                    && decode
                {
                    return Ok(Value::str(String::from_utf8_lossy(bytes).into_owned()));
                }
                Ok(Value::str(interpreter.to_str(&value)?))
            }
        },
        TypeTag::Bytes => match first {
            None => Ok(Value::Bytes(Rc::from(Vec::new()))),
            Some(value) => match value.plain() {
                Value::Str(text) => Ok(Value::Bytes(text.as_bytes().into())),
                Value::Bytes(_) => Ok(value.plain().clone()),
                Value::Int(size) => Ok(Value::Bytes(vec![0u8; (*size).max(0) as usize].into())),
                other => {
                    let items = interpreter.iterate(other)?;
                    Ok(Value::Bytes(
                        items
                            .iter()
                            .map(|item| item.as_int().unwrap_or(0) as u8)
                            .collect(),
                    ))
                }
            },
        },
        TypeTag::List => match first {
            None => Ok(Value::list(Vec::new())),
            Some(value) => Ok(Value::list(interpreter.iterate(&value)?)),
        },
        TypeTag::Tuple => match first {
            None => Ok(Value::tuple(Vec::new())),
            Some(value) => match value.plain() {
                Value::Tuple(_) => Ok(value.plain().clone()),
                other => Ok(Value::tuple(interpreter.iterate(other)?)),
            },
        },
        TypeTag::Set | TypeTag::FrozenSet => {
            let mut items = IndexSet::new();
            if let Some(value) = first {
                for item in interpreter.iterate(&value)? {
                    items.insert(Key::new(item.plain().clone())?);
                }
            }
            Ok(if tag == TypeTag::Set {
                Value::set(items)
            } else {
                Value::FrozenSet(Rc::new(items))
            })
        }
        TypeTag::Dict | TypeTag::OrderedDict | TypeTag::DefaultDict | TypeTag::Counter => {
            let mut dict = Dict::default();
            let mut positional = args.positional.clone();
            dict.kind = match tag {
                TypeTag::OrderedDict => DictKind::OrderedDict,
                TypeTag::DefaultDict => {
                    let factory = if positional.is_empty() {
                        Value::None
                    } else {
                        positional.remove(0)
                    };
                    DictKind::Default(factory)
                }
                TypeTag::Counter => DictKind::Counter,
                _ => DictKind::Plain,
            };
            if let Some(source) = positional.first() {
                if tag == TypeTag::Counter && !matches!(source.plain(), Value::Dict(_)) {
                    for item in interpreter.iterate(source)? {
                        let key = Key::new(item.plain().clone())?;
                        let count = dict.entries.get(&key).and_then(Value::as_int).unwrap_or(0);
                        dict.entries.insert(key, Value::Int(count + 1));
                    }
                } else {
                    update_dict(interpreter, &mut dict, source)?;
                }
            }
            for (key, value) in args.keywords {
                dict.entries.insert(Key(Value::Str(key)), value);
            }
            Ok(Value::dict(dict))
        }
        TypeTag::Range => {
            let numbers: Vec<i64> = args
                .positional
                .iter()
                .map(|value| {
                    value.as_int().ok_or_else(|| {
                        Error::type_error(format!(
                            "'{}' object cannot be interpreted as an integer",
                            types::type_name(value)
                        ))
                    })
                })
                .collect::<Result<_, _>>()?;
            let (start, stop, step) = match numbers.as_slice() {
                [stop] => (0, *stop, 1),
                [start, stop] => (*start, *stop, 1),
                [start, stop, step] => (*start, *stop, *step),
                _ => return Err(Error::type_error("range expected 1 to 3 arguments")),
            };
            if step == 0 {
                return Err(Error::value_error("range() arg 3 must not be zero"));
            }
            Ok(Value::Range(Rc::new(Range { start, stop, step })))
        }
        TypeTag::Slice => {
            let mut parts = args.positional.clone();
            let parts = match parts.len() {
                1 => [Value::None, parts.remove(0), Value::None],
                2 => [parts[0].clone(), parts[1].clone(), Value::None],
                3 => [parts[0].clone(), parts[1].clone(), parts[2].clone()],
                _ => return Err(Error::type_error("slice expected 1 to 3 arguments")),
            };
            Ok(Value::Slice(Rc::new(parts)))
        }
        other => Err(Error::type_error(format!(
            "cannot create '{}' instances",
            types::builtin_class(other).name
        ))),
    }
}

/// Adds the entries of a mapping or of an iterable of pairs to a dict.
pub fn update_dict(
    interpreter: &mut Interpreter,
    dict: &mut Dict,
    source: &Value,
) -> Result<(), Error> {
    let is_mapping = matches!(source.plain(), Value::Dict(_))
        || interpreter.find_attribute(source, "keys")?.is_some()
            && !matches!(source.plain(), Value::List(_) | Value::Tuple(_));
    if is_mapping {
        for (key, value) in interpreter.mapping_items(source)? {
            dict.entries.insert(Key::new(key.plain().clone())?, value);
        }
        return Ok(());
    }
    for pair in interpreter.iterate(source)? {
        let items = interpreter.iterate(&pair)?;
        if items.len() != 2 {
            return Err(Error::value_error(format!(
                "dictionary update sequence element has length {}; 2 is required",
                items.len()
            )));
        }
        dict.entries
            .insert(Key::new(items[0].plain().clone())?, items[1].clone());
    }
    Ok(())
}

pub fn to_int(interpreter: &mut Interpreter, value: &Value) -> Result<Value, Error> {
    match value.plain() {
        Value::Bool(value) => Ok(Value::Int(i64::from(*value))),
        Value::Int(_) | Value::BigInt(_) => Ok(value.plain().clone()),
        Value::Float(number) => number::float_to_int(*number),
        Value::Str(text) => number::parse_int(text, 10),
        Value::Bytes(bytes) => number::parse_int(&String::from_utf8_lossy(bytes), 10),
        other => {
            if let Some(method) = interpreter.lookup_special(other, "__int__") {
                return interpreter.call(&method, Args::default());
            }
            if let Value::Native(object) = other
                && let Some(result) = object.unary(interpreter, "int")?
            {
                return Ok(result);
            }
            Err(Error::type_error(format!(
                "int() argument must be a string, a bytes-like object or a real number, not '{}'",
                types::type_name(other)
            )))
        }
    }
}

fn abs(interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    let value = one(&args, "abs")?;
    match value.plain() {
        Value::Bool(b) => Ok(Value::Int(i64::from(*b))),
        Value::Int(i) => Ok(i.checked_abs().map(Value::Int).unwrap_or_else(|| {
            Value::from_bigint(num_bigint::BigInt::from(*i).magnitude().clone().into())
        })),
        Value::BigInt(i) => Ok(Value::from_bigint(i.magnitude().clone().into())),
        Value::Float(f) => Ok(Value::Float(f.abs())),
        other => match interpreter.lookup_special(other, "__abs__") {
            Some(method) => interpreter.call(&method, Args::default()),
            None => Err(Error::type_error(format!(
                "bad operand type for abs(): '{}'",
                types::type_name(other)
            ))),
        },
    }
}

fn all(interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    let value = one(&args, "all")?;
    for item in interpreter.iterate(&value)? {
        if !interpreter.truthy(&item)? {
            return Ok(Value::Bool(false));
        }
    }
    Ok(Value::Bool(true))
}

fn any(interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    let value = one(&args, "any")?;
    for item in interpreter.iterate(&value)? {
        if interpreter.truthy(&item)? {
            return Ok(Value::Bool(true));
        }
    }
    Ok(Value::Bool(false))
}

fn repr(interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    let value = one(&args, "repr")?;
    Ok(Value::str(interpreter.repr(&value)?))
}

fn radix(
    _interpreter: &mut Interpreter,
    args: Args,
    base: u32,
    prefix: &str,
) -> Result<Value, Error> {
    let value = one(&args, "hex")?;
    let number =
        number::to_bigint(&value).ok_or_else(|| Error::type_error("an integer is required"))?;
    let negative = number < num_bigint::BigInt::from(0);
    let digits = number.magnitude().to_str_radix(base);
    Ok(Value::str(format!(
        "{}{prefix}{digits}",
        if negative { "-" } else { "" }
    )))
}

fn callable(interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    let value = one(&args, "callable")?;
    Ok(Value::Bool(interpreter.callable(&value)))
}
