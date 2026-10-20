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
        TypeTag::Complex => {
            let real = args.get(0).or_else(|| args.keyword("real"));
            let imag = args.get(1).or_else(|| args.keyword("imag"));
            if real.is_some_and(|value| value.as_str().is_some()) {
                return Err(Error::unsupported("complex numbers from texts"));
            }
            crate::complex::Complex::construct(real, imag)
        }
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
        Value::Native(object) if object.as_any().is::<crate::complex::Complex>() => {
            let complex = object
                .as_any()
                .downcast_ref::<crate::complex::Complex>()
                .expect("a complex number");
            Ok(Value::Float(complex.0.hypot(complex.1)))
        }
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
    let iterator = interpreter.make_iterator(&value)?;
    loop {
        let item = interpreter.next_item(&iterator)?;
        let Some(item) = item else { break };
        if !interpreter.truthy(&item)? {
            return Ok(Value::Bool(false));
        }
    }
    Ok(Value::Bool(true))
}

fn any(interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    let value = one(&args, "any")?;
    let iterator = interpreter.make_iterator(&value)?;
    loop {
        let item = interpreter.next_item(&iterator)?;
        let Some(item) = item else { break };
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

fn chr(_interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    let value = one(&args, "chr")?;
    let code = value
        .as_int()
        .ok_or_else(|| Error::type_error("an integer is required"))?;
    char::from_u32(code as u32)
        .map(|c| Value::str(c.to_string()))
        .ok_or_else(|| Error::value_error("chr() arg not in range(0x110000)"))
}

/// `staticmethod`, `classmethod` and `property` objects around a function.
pub struct Wrapper(pub Value, pub &'static str);

impl Native for Wrapper {
    fn type_name(&self) -> &str {
        self.1
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        this: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        Ok(match (self.1, name) {
            ("property", "setter" | "deleter" | "getter") => {
                let this = this.clone();
                Some(Value::builtin("setter", move |_, _| Ok(this.clone())))
            }
            (_, "__func__" | "fget") => Some(self.0.clone()),
            _ => None,
        })
    }

    fn call(
        &self,
        interpreter: &mut Interpreter,
        _: &Value,
        args: Args,
    ) -> Result<Option<Value>, Error> {
        if self.1 == "staticmethod" {
            return interpreter.call(&self.0, args).map(Some);
        }
        Ok(None)
    }
}

fn wrap(args: Args, kind: &'static str) -> Result<Value, Error> {
    let function = args.positional.first().cloned().unwrap_or(Value::None);
    Ok(Value::native(Wrapper(function, kind)))
}

fn delattr(interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    args.expect("delattr", 2, 2)?;
    let name = args.positional[1].as_str().unwrap_or_default().to_owned();
    match args.positional[0].plain() {
        Value::Instance(instance) => {
            instance.dict.borrow_mut().shift_remove(name.as_str());
        }
        Value::Module(module) => {
            module.globals.vars.borrow_mut().shift_remove(name.as_str());
        }
        _ => return Err(Error::unsupported("delattr() on this object")),
    }
    let _ = interpreter;
    Ok(Value::None)
}

fn divmod(interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    args.expect("divmod", 2, 2)?;
    let (a, b) = (&args.positional[0], &args.positional[1]);
    let quotient = interpreter.binary(crate::interpreter::Operator::FloorDiv, a, b)?;
    let remainder = interpreter.binary(crate::interpreter::Operator::Mod, a, b)?;
    Ok(Value::tuple(vec![quotient, remainder]))
}

fn enumerate(interpreter: &mut Interpreter, mut args: Args) -> Result<Value, Error> {
    let start = args
        .take(1, "start")
        .and_then(|start| start.as_int())
        .unwrap_or(0);
    let iterable = args
        .take(0, "iterable")
        .ok_or_else(|| Error::type_error("enumerate() missing required argument"))?;
    let items = interpreter.iterate(&iterable)?;
    Ok(Value::iterator(items.into_iter().enumerate().map(
        |(index, item)| Value::tuple(vec![Value::Int(start + index as i64), item]),
    )))
}

fn exit(_interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    let class = types::exception_class("SystemExit").expect("SystemExit exists");
    let code = args.positional.first().cloned().unwrap_or(Value::None);
    Err(Error::from_value(crate::error::exception(
        &class,
        vec![code],
    )))
}

fn filter(interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    args.expect("filter", 2, 2)?;
    let function = args.positional[0].clone();
    let mut kept = Vec::new();
    for item in interpreter.iterate(&args.positional[1])? {
        let keep = if function.is_none() {
            interpreter.truthy(&item)?
        } else {
            let result = interpreter.call(&function, Args::new(vec![item.clone()]))?;
            interpreter.truthy(&result)?
        };
        if keep {
            kept.push(item);
        }
    }
    Ok(Value::iterator(kept))
}

fn format(interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    args.expect("format", 1, 2)?;
    let spec = args
        .positional
        .get(1)
        .and_then(|spec| spec.as_str().map(str::to_owned))
        .unwrap_or_default();
    Ok(Value::str(crate::format::format_value(
        interpreter,
        &args.positional[0],
        &spec,
    )?))
}

fn getattr(interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    args.expect("getattr", 2, 3)?;
    let name = args.positional[1]
        .as_str()
        .ok_or_else(|| Error::type_error("attribute name must be string"))?
        .to_owned();
    // With a default, an object that raises AttributeError gives the default too.
    match (
        interpreter.find_attribute(&args.positional[0], &name),
        args.positional.get(2),
    ) {
        (Ok(Some(value)), _) => Ok(value),
        (Ok(None), Some(default)) => Ok(default.clone()),
        (Err(error), Some(default)) if error.is("AttributeError") => Ok(default.clone()),
        (Ok(None), None) => interpreter.get_attribute(&args.positional[0], &name),
        (Err(error), _) => Err(error),
    }
}

fn hasattr(interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    args.expect("hasattr", 2, 2)?;
    let name = args.positional[1]
        .as_str()
        .ok_or_else(|| Error::type_error("attribute name must be string"))?
        .to_owned();
    match interpreter.find_attribute(&args.positional[0], &name) {
        Ok(found) => Ok(Value::Bool(found.is_some())),
        Err(error) if error.is("AttributeError") => Ok(Value::Bool(false)),
        Err(error) => Err(error),
    }
}

fn hash(interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    use std::hash::{Hash, Hasher};
    let value = one(&args, "hash")?;
    let key = interpreter.key(&value)?;
    if let Some(hash) = crate::hashing::stable_hash(&value) {
        return Ok(Value::Int(hash));
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut hasher);
    Ok(Value::Int((hasher.finish() >> 2) as i64))
}

fn id(_interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    let value = one(&args, "id")?;
    Ok(Value::Int(value.address() as i64))
}

fn isinstance(_: &mut Interpreter, args: Args) -> Result<Value, Error> {
    args.expect("isinstance", 2, 2)?;
    Ok(Value::Bool(instance_check(
        &args.positional[0],
        &args.positional[1],
    )?))
}

fn instance_check(value: &Value, kind: &Value) -> Result<bool, Error> {
    match kind.plain() {
        Value::Class(class) => Ok(types::is_instance(value, class)),
        Value::Tuple(kinds) => {
            for kind in kinds.iter() {
                if instance_check(value, kind)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        Value::Native(object) => match object.check_instance(value) {
            Some(found) => Ok(found),
            None => Err(Error::type_error(format!(
                "isinstance() arg 2 must be a type, a tuple of types, or a union, not {}",
                object.type_name()
            ))),
        },
        other => Err(Error::type_error(format!(
            "isinstance() arg 2 must be a type, a tuple of types, or a union, not {}",
            types::type_name(other)
        ))),
    }
}

fn issubclass(_interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    args.expect("issubclass", 2, 2)?;
    let Value::Class(class) = args.positional[0].plain() else {
        return Err(Error::type_error("issubclass() arg 1 must be a class"));
    };
    let kinds = match args.positional[1].plain() {
        Value::Tuple(kinds) => kinds.to_vec(),
        other => vec![other.clone()],
    };
    Ok(Value::Bool(kinds.iter().any(
        |kind| matches!(kind.plain(), Value::Class(kind) if class.is_subclass(kind)),
    )))
}

fn iter(interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    if args.positional.len() == 2 {
        return Err(Error::unsupported("iter() with a sentinel"));
    }
    let value = one(&args, "iter")?;
    interpreter.make_iterator(&value)
}

fn len(interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    let value = one(&args, "len")?;
    Ok(Value::Int(interpreter.length(&value)? as i64))
}

fn map(interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    args.expect("map", 2, usize::MAX)?;
    let function = args.positional[0].clone();
    let lists = args.positional[1..]
        .iter()
        .map(|iterable| interpreter.iterate(iterable))
        .collect::<Result<Vec<_>, _>>()?;
    let count = lists.iter().map(Vec::len).min().unwrap_or(0);
    let mut results = Vec::with_capacity(count);
    for index in 0..count {
        let call_args: Vec<Value> = lists.iter().map(|list| list[index].clone()).collect();
        results.push(interpreter.call(&function, Args::new(call_args))?);
    }
    Ok(Value::iterator(results))
}

fn extreme(
    interpreter: &mut Interpreter,
    mut args: Args,
    wanted: Ordering,
    name: &str,
) -> Result<Value, Error> {
    let key = args.take_keyword("key").filter(|key| !key.is_none());
    let default = args.take_keyword("default");
    let items = if args.positional.len() == 1 {
        interpreter.iterate(&args.positional[0])?
    } else {
        args.positional.clone()
    };
    let mut best: Option<(Value, Value)> = None;
    for item in items {
        let measure = match &key {
            Some(key) => interpreter.call(key, Args::new(vec![item.clone()]))?,
            None => item.clone(),
        };
        let better = match &best {
            None => true,
            Some((_, best_measure)) => interpreter.compare(&measure, best_measure)? == wanted,
        };
        if better {
            best = Some((item, measure));
        }
    }
    match best {
        Some((item, _)) => Ok(item),
        None => {
            default.ok_or_else(|| Error::value_error(format!("{name}() arg is an empty sequence")))
        }
    }
}

fn next(interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    args.expect("next", 1, 2)?;
    let iterator = &args.positional[0];
    let found = match iterator.plain() {
        Value::Iterator(_) => interpreter.next_item(iterator)?,
        Value::Instance(_) => match interpreter.lookup_special(iterator, "__next__") {
            Some(method) => match interpreter.call(&method, Args::default()) {
                Ok(value) => Some(value),
                Err(error) if error.is("StopIteration") => None,
                Err(error) => return Err(error),
            },
            None => None,
        },
        // Native iterators like files and csv readers have `__next__`.
        Value::Native(object) => {
            let object = object.clone();
            match object.get_attribute(interpreter, iterator, "__next__")? {
                Some(method) => match interpreter.call(&method, Args::default()) {
                    Ok(value) => Some(value),
                    Err(error) if error.is("StopIteration") => None,
                    Err(error) => return Err(error),
                },
                None => {
                    return Err(Error::type_error(format!(
                        "'{}' object is not an iterator",
                        object.type_name()
                    )));
                }
            }
        }
        other => {
            return Err(Error::type_error(format!(
                "'{}' object is not an iterator",
                types::type_name(other)
            )));
        }
    };
    match (found, args.positional.get(1)) {
        (Some(value), _) => Ok(value),
        (None, Some(default)) => Ok(default.clone()),
        (None, None) => Err(Error::stop_iteration()),
    }
}

fn ord(_interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    let value = one(&args, "ord")?;
    let text = value
        .as_str()
        .ok_or_else(|| Error::type_error("ord() expected string of length 1"))?;
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Ok(Value::Int(c as i64)),
        _ => Err(Error::type_error(format!(
            "ord() expected a character, but string of length {} found",
            text.chars().count()
        ))),
    }
}

fn pow(interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    args.expect("pow", 2, 3)?;
    let result = interpreter.binary(
        crate::interpreter::Operator::Pow,
        &args.positional[0],
        &args.positional[1],
    )?;
    match args.positional.get(2) {
        Some(modulus) => interpreter.binary(crate::interpreter::Operator::Mod, &result, modulus),
        None => Ok(result),
    }
}

fn print(interpreter: &mut Interpreter, mut args: Args) -> Result<Value, Error> {
    let separator = match args.take_keyword("sep") {
        Some(Value::Str(text)) => text.to_string(),
        _ => " ".into(),
    };
    let end = match args.take_keyword("end") {
        Some(Value::Str(text)) => text.to_string(),
        _ => "\n".into(),
    };
    let file = args.take_keyword("file");
    args.take_keyword("flush");
    let mut parts = Vec::new();
    for value in &args.positional {
        parts.push(interpreter.to_str(value)?);
    }
    let text = format!("{}{end}", parts.join(&separator));
    match file {
        Some(file) if !file.is_none() => {
            let write = interpreter.get_attribute(&file, "write")?;
            interpreter.call(&write, Args::new(vec![Value::str(text)]))?;
        }
        _ => interpreter.host.print(&text),
    }
    Ok(Value::None)
}

fn reversed(interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    let value = one(&args, "reversed")?;
    if let Some(method) = interpreter.lookup_special(&value, "__reversed__") {
        return interpreter.call(&method, Args::default());
    }
    let mut items = interpreter.iterate(&value)?;
    items.reverse();
    Ok(Value::iterator(items))
}

fn round(interpreter: &mut Interpreter, mut args: Args) -> Result<Value, Error> {
    let digits = args.take(1, "ndigits").filter(|digits| !digits.is_none());
    let value = args
        .take(0, "number")
        .ok_or_else(|| Error::type_error("round() missing required argument"))?;
    match (value.plain(), digits) {
        (Value::Int(_) | Value::BigInt(_) | Value::Bool(_), None) => to_int(interpreter, &value),
        (Value::Int(_) | Value::BigInt(_) | Value::Bool(_), Some(digits)) => {
            let digits = digits.as_int().unwrap_or(0);
            if digits >= 0 {
                return to_int(interpreter, &value);
            }
            let factor = 10f64.powi((-digits) as i32);
            let number = to_f64(&value).unwrap_or(0.0);
            number::float_to_int(number::round_half_even(number / factor) * factor)
        }
        (Value::Float(number), None) => number::float_to_int(number::round_half_even(*number)),
        (Value::Float(number), Some(digits)) => {
            let digits = digits.as_int().unwrap_or(0);
            if digits >= 0 {
                let text = format!("{number:.*}", digits as usize);
                Ok(Value::Float(text.parse().unwrap_or(*number)))
            } else {
                let factor = 10f64.powi((-digits) as i32);
                Ok(Value::Float(
                    number::round_half_even(number / factor) * factor,
                ))
            }
        }
        (other, digits) => match interpreter.lookup_special(other, "__round__") {
            Some(method) => {
                let args = digits.map(|digits| vec![digits]).unwrap_or_default();
                interpreter.call(&method, Args::new(args))
            }
            None => Err(Error::type_error(format!(
                "type {} doesn't define __round__ method",
                types::type_name(other)
            ))),
        },
    }
}

fn setattr(interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    args.expect("setattr", 3, 3)?;
    let name = args.positional[1]
        .as_str()
        .ok_or_else(|| Error::type_error("attribute name must be string"))?
        .to_owned();
    interpreter.set_attribute(&args.positional[0], &name, args.positional[2].clone())?;
    Ok(Value::None)
}

/// Sorts values stably by an optional key; the first failed comparison is the error.
pub fn sort_values(
    interpreter: &mut Interpreter,
    items: Vec<Value>,
    key: Option<&Value>,
    reverse: bool,
) -> Result<Vec<Value>, Error> {
    let mut keyed = Vec::with_capacity(items.len());
    for item in items {
        let measure = match key {
            Some(key) => interpreter.call(key, Args::new(vec![item.clone()]))?,
            None => item.clone(),
        };
        keyed.push((measure, item));
    }
    let mut failure = None;
    keyed.sort_by(|(a, _), (b, _)| {
        if failure.is_some() {
            return Ordering::Equal;
        }
        let ordering = match interpreter.compare(a, b) {
            Ok(ordering) => ordering,
            Err(error) => {
                failure = Some(error);
                Ordering::Equal
            }
        };
        if reverse {
            ordering.reverse()
        } else {
            ordering
        }
    });
    if let Some(error) = failure {
        return Err(error);
    }
    Ok(keyed.into_iter().map(|(_, item)| item).collect())
}

fn sorted(interpreter: &mut Interpreter, mut args: Args) -> Result<Value, Error> {
    let key = args.take_keyword("key").filter(|key| !key.is_none());
    let reverse = match args.take_keyword("reverse") {
        Some(value) => interpreter.truthy(&value)?,
        None => false,
    };
    args.expect("sorted", 1, 1)?;
    let items = interpreter.iterate(&args.positional[0])?;
    Ok(Value::list(sort_values(
        interpreter,
        items,
        key.as_ref(),
        reverse,
    )?))
}

fn sum(interpreter: &mut Interpreter, mut args: Args) -> Result<Value, Error> {
    let start = args.take(1, "start").unwrap_or(Value::Int(0));
    let iterable = args
        .take(0, "iterable")
        .ok_or_else(|| Error::type_error("sum() missing argument"))?;
    let mut total = start;
    for item in interpreter.iterate(&iterable)? {
        total = interpreter.binary(crate::interpreter::Operator::Add, &total, &item)?;
    }
    Ok(total)
}

fn vars(interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    let value = one(&args, "vars")?;
    interpreter.get_attribute(&value, "__dict__")
}

fn dir(interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    let value = one(&args, "dir")?;
    let mut names: Vec<String> = match value.plain() {
        Value::Module(module) => module
            .globals
            .vars
            .borrow()
            .keys()
            .map(|name| name.to_string())
            .collect(),
        Value::Instance(instance) => {
            let mut names: Vec<String> = instance
                .dict
                .borrow()
                .keys()
                .map(|name| name.to_string())
                .collect();
            for class in instance.class.mro.borrow().iter() {
                names.extend(class.dict.borrow().keys().map(|name| name.to_string()));
            }
            names
        }
        Value::Class(class) => class
            .dict
            .borrow()
            .keys()
            .map(|name| name.to_string())
            .collect(),
        _ => Vec::new(),
    };
    let _ = interpreter;
    names.sort();
    names.dedup();
    Ok(Value::list(names.into_iter().map(Value::str).collect()))
}

fn zip(interpreter: &mut Interpreter, mut args: Args) -> Result<Value, Error> {
    let strict = args
        .take_keyword("strict")
        .is_some_and(|strict| matches!(strict, Value::Bool(true)));
    let lists = args
        .positional
        .iter()
        .map(|iterable| interpreter.iterate(iterable))
        .collect::<Result<Vec<_>, _>>()?;
    let count = lists.iter().map(Vec::len).min().unwrap_or(0);
    if strict && lists.iter().any(|list| list.len() != count) {
        return Err(Error::value_error("zip() arguments have different lengths"));
    }
    Ok(Value::iterator((0..count).map(|index| {
        Value::tuple(lists.iter().map(|list| list[index].clone()).collect())
    })))
}

fn super_(_interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    args.expect("super", 2, 2)?;
    let Value::Class(class) = args.positional[0].plain() else {
        return Err(Error::type_error("super() argument 1 must be a type"));
    };
    Ok(make_super(class.clone(), args.positional[1].clone()))
}

fn import(interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    let name = one(&args, "__import__")?;
    let name = name
        .as_str()
        .ok_or_else(|| Error::type_error("module name must be str"))?
        .to_owned();
    interpreter.import_module(&name)
}

/// `super()`: attributes of the classes after `class` in the receiver's class order.
pub fn make_super(class: Rc<Class>, receiver: Value) -> Value {
    Value::native(Super { class, receiver })
}

struct Super {
    class: Rc<Class>,
    receiver: Value,
}

impl Native for Super {
    fn type_name(&self) -> &str {
        "super"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get_attribute(
        &self,
        interpreter: &mut Interpreter,
        _: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        let receiver_class = match self.receiver.plain() {
            Value::Class(class) => class.clone(),
            other => types::class_of(other),
        };
        let mro = receiver_class.mro.borrow().clone();
        let start = mro
            .iter()
            .position(|class| Rc::ptr_eq(class, &self.class))
            .map_or(mro.len(), |index| index + 1);
        for class in &mro[start..] {
            if let Some(found) = class.dict.borrow().get(name) {
                return Ok(Some(bind_method(&self.receiver, found.clone())));
            }
        }
        // The methods of `object` and of builtin exceptions.
        let receiver = self.receiver.clone();
        Ok(match name {
            "__init__" => Some(Value::builtin("__init__", move |_, args| {
                if types::is_exception(&receiver)
                    && let Value::Instance(instance) = receiver.plain()
                {
                    instance
                        .dict
                        .borrow_mut()
                        .insert("args".into(), Value::tuple(args.positional));
                }
                Ok(Value::None)
            })),
            "__str__" | "__repr__" => {
                let receiver = self.receiver.clone();
                let repr = name == "__repr__";
                Some(Value::builtin(name, move |_, _| {
                    Ok(Value::str(if repr {
                        crate::format::plain_repr(&receiver)
                    } else {
                        crate::format::plain_str(&receiver)
                    }))
                }))
            }
            "__setattr__" => {
                let receiver = self.receiver.clone();
                Some(Value::builtin("__setattr__", move |interpreter, args| {
                    args.expect("__setattr__", 2, 2)?;
                    if let Value::Instance(instance) = receiver.plain() {
                        let name = args.positional[0].as_str().unwrap_or_default();
                        instance
                            .dict
                            .borrow_mut()
                            .insert(name.into(), args.positional[1].clone());
                    }
                    let _ = interpreter;
                    Ok(Value::None)
                }))
            }
            _ => {
                let _ = interpreter;
                None
            }
        })
    }
}

/// Attributes of builtin classes, like `str.join` or `dict.fromkeys`, and the methods of
/// `object` and of builtin exceptions that subclasses call.
pub fn class_attribute(
    interpreter: &mut Interpreter,
    class: &Rc<Class>,
    name: &str,
) -> Result<Option<Value>, Error> {
    let _ = interpreter;
    match (class.builtin, name) {
        (Some(TypeTag::BaseException), "__init__") | (Some(TypeTag::Object), "__init__") => {
            return Ok(Some(Value::builtin("__init__", |_, args| {
                if let Some(receiver) = args.positional.first()
                    && types::is_exception(receiver)
                    && let Value::Instance(instance) = receiver.plain()
                {
                    instance
                        .dict
                        .borrow_mut()
                        .insert("args".into(), Value::tuple(args.positional[1..].to_vec()));
                }
                Ok(Value::None)
            })));
        }
        (_, "__setattr__") => {
            return Ok(Some(Value::builtin("__setattr__", |interpreter, args| {
                args.expect("__setattr__", 3, 3)?;
                if let Value::Instance(instance) = args.positional[0].plain() {
                    let name = args.positional[1].as_str().unwrap_or_default();
                    instance
                        .dict
                        .borrow_mut()
                        .insert(name.into(), args.positional[2].clone());
                    return Ok(Value::None);
                }
                let name = args.positional[1].as_str().unwrap_or_default().to_owned();
                interpreter.set_attribute(
                    &args.positional[0],
                    &name,
                    args.positional[2].clone(),
                )?;
                Ok(Value::None)
            })));
        }
        (_, "__new__") => {
            return Ok(Some(Value::builtin("__new__", |_, args| {
                let Some(Value::Class(class)) =
                    args.positional.first().map(|value| value.plain().clone())
                else {
                    return Err(Error::type_error(
                        "object.__new__(X): X is not a type object",
                    ));
                };
                Ok(Value::Instance(Rc::new(crate::value::Instance {
                    class,
                    dict: RefCell::new(IndexMap::new()),
                })))
            })));
        }
        _ => {}
    }
    let Some(tag) = class.builtin else {
        return Ok(None);
    };
    if matches!(
        tag,
        TypeTag::Dict | TypeTag::OrderedDict | TypeTag::DefaultDict | TypeTag::Counter
    ) && name == "fromkeys"
    {
        return Ok(Some(Value::builtin(
            "fromkeys",
            move |interpreter, args| {
                args.expect("fromkeys", 1, 2)?;
                let value = args.positional.get(1).cloned().unwrap_or(Value::None);
                let mut dict = Dict::default();
                for key in interpreter.iterate(&args.positional[0])? {
                    dict.entries
                        .insert(Key::new(key.plain().clone())?, value.clone());
                }
                Ok(Value::dict(dict))
            },
        )));
    }
    if tag == TypeTag::Str && name == "maketrans" {
        return Ok(Some(Value::builtin("maketrans", crate::methods::maketrans)));
    }
    let sample = match tag {
        TypeTag::Str => Value::str(""),
        TypeTag::List => Value::list(Vec::new()),
        TypeTag::Dict => Value::dict(Dict::default()),
        TypeTag::Set => Value::set(IndexSet::new()),
        TypeTag::Tuple => Value::tuple(Vec::new()),
        TypeTag::Int => Value::Int(0),
        TypeTag::Float => Value::Float(0.0),
        TypeTag::Bytes => Value::Bytes(Rc::from(Vec::new())),
        _ => return Ok(None),
    };
    if !crate::methods::has_method(&sample, name) {
        return Ok(None);
    }
    let method: Rc<str> = name.into();
    Ok(Some(Value::builtin(name, move |interpreter, mut args| {
        if args.positional.is_empty() {
            return Err(Error::type_error(format!(
                "unbound method {method}() needs an argument"
            )));
        }
        let receiver = args.positional.remove(0);
        crate::methods::call(interpreter, &receiver, &method, args)
    })))
}

/// `function.__code__`, with the names that introspection asks for.
pub fn code_object(function: &Rc<Function>) -> Value {
    let names: Vec<Value> = function
        .code
        .parameter_names()
        .into_iter()
        .map(Value::str)
        .collect();
    let count = function.code.parameters().map_or(0, |parameters| {
        parameters.posonlyargs.len() + parameters.args.len()
    });
    let mut dict = Dict::default();
    dict.insert("co_varnames", Value::tuple(names));
    dict.insert("co_argcount", Value::Int(count as i64));
    dict.insert("co_name", Value::Str(function.name.clone()));
    dict.insert("co_filename", Value::Str(function.source.path.clone()));
    Value::native(Record(RefCell::new(dict), "code"))
}

/// An object whose attributes are the entries of a dict.
pub struct Record(pub RefCell<Dict>, pub &'static str);

impl Native for Record {
    fn type_name(&self) -> &str {
        self.1
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        _: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        Ok(self.0.borrow().get(name).cloned())
    }

    fn set_attribute(&self, _: &mut Interpreter, name: &str, value: Value) -> Result<bool, Error> {
        self.0.borrow_mut().insert(name, value);
        Ok(true)
    }
}

/// `globals()`: reads and writes the variables of a module.
pub struct GlobalsDict(pub Rc<Scope>);

impl Native for GlobalsDict {
    fn type_name(&self) -> &str {
        "dict"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn instance_of(&self, class: &str) -> bool {
        class == "dict"
    }

    fn get_item(
        &self,
        _: &mut Interpreter,
        _: &Value,
        key: &Value,
    ) -> Result<Option<Value>, Error> {
        let name = key.as_str().unwrap_or_default();
        match self.0.get(name) {
            Some(value) => Ok(Some(value)),
            None => Err(Error::key_error(key.clone())),
        }
    }

    fn set_item(&self, _: &mut Interpreter, key: &Value, value: Value) -> Result<bool, Error> {
        let name = key
            .as_str()
            .ok_or_else(|| Error::type_error("global names must be strings"))?;
        self.0.set(name, value);
        Ok(true)
    }

    fn contains(&self, _: &mut Interpreter, item: &Value) -> Result<Option<bool>, Error> {
        Ok(Some(item.as_str().is_some_and(|name| {
            self.0.vars.borrow().contains_key(name)
        })))
    }

    fn iterate(&self, _: &mut Interpreter) -> Result<Option<Vec<Value>>, Error> {
        Ok(Some(
            self.0
                .vars
                .borrow()
                .keys()
                .map(|name| Value::Str(name.clone()))
                .collect(),
        ))
    }

    fn length(&self) -> Option<usize> {
        Some(self.0.vars.borrow().len())
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        _: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        let scope = self.0.clone();
        let snapshot = || {
            let vars = scope.vars.borrow();
            Value::dict_of(
                vars.iter()
                    .map(|(name, value)| (name.to_string(), value.clone())),
            )
        };
        Ok(match name {
            "get" => {
                let scope = scope.clone();
                Some(Value::builtin("get", move |_, args| {
                    args.expect("get", 1, 2)?;
                    let name = args.positional[0].as_str().unwrap_or_default();
                    Ok(scope
                        .get(name)
                        .unwrap_or_else(|| args.positional.get(1).cloned().unwrap_or(Value::None)))
                }))
            }
            "update" => {
                let scope = scope.clone();
                Some(Value::builtin("update", move |interpreter, args| {
                    if let Some(mapping) = args.positional.first() {
                        for (key, value) in interpreter.mapping_items(mapping)? {
                            if let Some(name) = key.as_str() {
                                scope.set(name, value);
                            }
                        }
                    }
                    for (name, value) in args.keywords {
                        scope.set(&name, value);
                    }
                    Ok(Value::None)
                }))
            }
            "setdefault" => {
                let scope = scope.clone();
                Some(Value::builtin("setdefault", move |_, args| {
                    args.expect("setdefault", 1, 2)?;
                    let name = args.positional[0].as_str().unwrap_or_default();
                    if let Some(value) = scope.get(name) {
                        return Ok(value);
                    }
                    let value = args.positional.get(1).cloned().unwrap_or(Value::None);
                    scope.set(name, value.clone());
                    Ok(value)
                }))
            }
            "keys" | "values" | "items" | "copy" => {
                let dict = snapshot();
                let method: Rc<str> = name.into();
                Some(Value::builtin(name, move |interpreter, args| {
                    crate::methods::call(interpreter, &dict, &method, args)
                }))
            }
            _ => None,
        })
    }
}

/// An open file: text read at once, or text kept until the file is closed.
pub struct File {
    pub path: String,
    pub mode: String,
    pub content: RefCell<Vec<u8>>,
    pub position: RefCell<usize>,
    pub written: RefCell<Vec<u8>>,
    pub closed: RefCell<bool>,
    /// Writes go to the host's output instead of a file, for `sys.stdout` and `sys.stderr`.
    pub console: bool,
    pub compressed: bool,
}

impl File {
    fn binary(&self) -> bool {
        self.mode.contains('b')
    }

    fn writing(&self) -> bool {
        self.mode.contains(['w', 'a', 'x', '+'])
    }

    fn text(&self, bytes: &[u8]) -> Value {
        if self.binary() {
            Value::Bytes(bytes.into())
        } else {
            Value::str(String::from_utf8_lossy(bytes).into_owned())
        }
    }

    fn read_line(&self) -> Option<Vec<u8>> {
        let content = self.content.borrow();
        let mut position = self.position.borrow_mut();
        if *position >= content.len() {
            return None;
        }
        let rest = &content[*position..];
        let end = rest
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(rest.len(), |index| index + 1);
        *position += end;
        Some(rest[..end].to_vec())
    }

    fn flush(&self, interpreter: &mut Interpreter) -> Result<(), Error> {
        if *self.closed.borrow() || !self.writing() {
            return Ok(());
        }
        let data = self.written.borrow().clone();
        if self.console {
            interpreter.host.print(&String::from_utf8_lossy(&data));
            self.written.borrow_mut().clear();
            return Ok(());
        }
        let append = self.mode.contains('a');
        let path = interpreter.host.resolve(&self.path);
        interpreter
            .host
            .write(&path, &data, append)
            .map_err(|error| Error::os_error(&error, &self.path))?;
        if append {
            self.written.borrow_mut().clear();
        }
        Ok(())
    }
}

impl Native for File {
    fn type_name(&self) -> &str {
        "TextIOWrapper"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn iterate(&self, _: &mut Interpreter) -> Result<Option<Vec<Value>>, Error> {
        let mut lines = Vec::new();
        while let Some(line) = self.read_line() {
            lines.push(self.text(&line));
        }
        Ok(Some(lines))
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        this: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        Ok(match name {
            "name" => Some(Value::str(self.path.as_str())),
            "mode" => Some(Value::str(self.mode.as_str())),
            "closed" => Some(Value::Bool(*self.closed.borrow())),
            _ => file_method(this, name),
        })
    }
}

/// A method of a file object, bound to the file value.
pub fn file_method(file_value: &Value, name: &str) -> Option<Value> {
    let file_value = file_value.clone();
    let method: Rc<str> = name.into();
    let known = matches!(
        name,
        "read"
            | "readline"
            | "readlines"
            | "write"
            | "writelines"
            | "close"
            | "flush"
            | "__enter__"
            | "__exit__"
            | "__iter__"
            | "__next__"
            | "seek"
            | "tell"
            | "fileno"
    );
    if !known {
        return None;
    }
    Some(Value::builtin(name, move |interpreter, args| {
        let Some(file) = file_value.as_native::<File>() else {
            return Err(Error::type_error("not a file"));
        };
        match &*method {
            "read" => {
                let size = args
                    .positional
                    .first()
                    .and_then(Value::as_int)
                    .filter(|size| *size >= 0);
                let content = file.content.borrow();
                let mut position = file.position.borrow_mut();
                let end = match size {
                    Some(size) => (*position + size as usize).min(content.len()),
                    None => content.len(),
                };
                let start = (*position).min(content.len());
                *position = end;
                Ok(file.text(&content[start..end]))
            }
            "readline" => Ok(file
                .read_line()
                .map_or_else(|| file.text(&[]), |line| file.text(&line))),
            "readlines" => {
                let mut lines = Vec::new();
                while let Some(line) = file.read_line() {
                    lines.push(file.text(&line));
                }
                Ok(Value::list(lines))
            }
            "__next__" => match file.read_line() {
                Some(line) => Ok(file.text(&line)),
                None => Err(Error::stop_iteration()),
            },
            "write" => {
                args.expect("write", 1, 1)?;
                let data = match args.positional[0].plain() {
                    Value::Str(text) => text.as_bytes().to_vec(),
                    Value::Bytes(bytes) => bytes.to_vec(),
                    other => {
                        return Err(Error::type_error(format!(
                            "write() argument must be str, not {}",
                            types::type_name(other)
                        )));
                    }
                };
                let length = data.len();
                file.written.borrow_mut().extend(data);
                if file.console {
                    file.flush(interpreter)?;
                }
                Ok(Value::Int(length as i64))
            }
            "writelines" => {
                args.expect("writelines", 1, 1)?;
                for line in interpreter.iterate(&args.positional[0])? {
                    let text = interpreter.to_str(&line)?;
                    file.written.borrow_mut().extend(text.as_bytes());
                }
                Ok(Value::None)
            }
            "close" | "__exit__" => {
                file.flush(interpreter)?;
                if !file.console {
                    *file.closed.borrow_mut() = true;
                }
                Ok(if &*method == "__exit__" {
                    Value::Bool(false)
                } else {
                    Value::None
                })
            }
            "flush" => {
                file.flush(interpreter)?;
                Ok(Value::None)
            }
            "__enter__" | "__iter__" => Ok(file_value.clone()),
            "seek" => {
                let offset = args.positional.first().and_then(Value::as_int).unwrap_or(0);
                *file.position.borrow_mut() = offset.max(0) as usize;
                Ok(Value::Int(offset))
            }
            "tell" => Ok(Value::Int(*file.position.borrow() as i64)),
            "fileno" => Err(Error::unsupported("file descriptors")),
            _ => Ok(Value::None),
        }
    }))
}

/// `open(path, mode)`: reading gets the whole content now; writing keeps the text in the
/// host's memory when the file is closed.
fn open(interpreter: &mut Interpreter, mut args: Args) -> Result<Value, Error> {
    let path = args
        .take(0, "file")
        .ok_or_else(|| Error::type_error("open() missing required argument 'file'"))?;
    let mode = args
        .take(1, "mode")
        .and_then(|mode| mode.as_str().map(str::to_owned))
        .unwrap_or_else(|| "r".into());
    let path = crate::modules::path_text(interpreter, &path)?;
    open_file(interpreter, &path, &mode, false)
}

pub fn open_file(
    interpreter: &mut Interpreter,
    path: &str,
    mode: &str,
    compressed: bool,
) -> Result<Value, Error> {
    let resolved = interpreter.host.resolve(path);
    let reading = !mode.contains(['w', 'a', 'x']);
    let content = if reading {
        let data = interpreter
            .host
            .read(&resolved)
            .map_err(|error| Error::os_error(&error, path))?;
        if compressed {
            use std::io::Read;
            let mut decoded = Vec::new();
            flate2::read::MultiGzDecoder::new(&data[..])
                .read_to_end(&mut decoded)
                .map_err(|_| Error::new("OSError", format!("Not a gzipped file: '{path}'")))?;
            decoded
        } else {
            data
        }
    } else {
        if mode.contains('x') && interpreter.host.stat(&resolved).is_some() {
            return Err(Error::os_error(
                &std::io::ErrorKind::AlreadyExists.into(),
                path,
            ));
        }
        Vec::new()
    };
    let mode = if compressed && !mode.contains('t') && !mode.contains('b') {
        format!("{mode}b")
    } else {
        mode.replace('t', "")
    };
    Ok(Value::native(File {
        path: path.to_owned(),
        mode,
        content: RefCell::new(content),
        position: RefCell::new(0),
        written: RefCell::new(Vec::new()),
        closed: RefCell::new(false),
        console: false,
        compressed,
    }))
}
