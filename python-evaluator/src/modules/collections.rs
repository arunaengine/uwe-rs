//! `itertools`, `functools`, `collections`, `operator`, `copy`, `heapq` and `bisect`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;

use indexmap::IndexMap;

use super::function;
use crate::builtins::sort_values;
use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::types::{self, TypeTag};
use crate::value::{Args, Dict, Instance, Iter, Key, Native, Value};

fn lists(interpreter: &mut Interpreter, values: &[Value]) -> Result<Vec<Vec<Value>>, Error> {
    values
        .iter()
        .map(|value| interpreter.iterate(value))
        .collect()
}

fn product(pools: &[Vec<Value>]) -> Vec<Value> {
    let mut results: Vec<Vec<Value>> = vec![Vec::new()];
    for pool in pools {
        let mut next = Vec::with_capacity(results.len() * pool.len());
        for prefix in &results {
            for item in pool {
                let mut combination = prefix.clone();
                combination.push(item.clone());
                next.push(combination);
            }
        }
        results = next;
    }
    results.into_iter().map(Value::tuple).collect()
}

fn combinations(pool: &[Value], size: usize, replace: bool) -> Vec<Value> {
    let mut results = Vec::new();
    let mut current = Vec::new();
    fn walk(
        pool: &[Value],
        size: usize,
        start: usize,
        replace: bool,
        current: &mut Vec<Value>,
        results: &mut Vec<Value>,
    ) {
        if current.len() == size {
            results.push(Value::tuple(current.clone()));
            return;
        }
        for index in start..pool.len() {
            current.push(pool[index].clone());
            walk(
                pool,
                size,
                if replace { index } else { index + 1 },
                replace,
                current,
                results,
            );
            current.pop();
        }
    }
    walk(pool, size, 0, replace, &mut current, &mut results);
    results
}

fn permutations(pool: &[Value], size: usize) -> Vec<Value> {
    let mut results = Vec::new();
    let mut used = vec![false; pool.len()];
    let mut current = Vec::new();
    fn walk(
        pool: &[Value],
        size: usize,
        used: &mut Vec<bool>,
        current: &mut Vec<Value>,
        results: &mut Vec<Value>,
    ) {
        if current.len() == size {
            results.push(Value::tuple(current.clone()));
            return;
        }
        for index in 0..pool.len() {
            if used[index] {
                continue;
            }
            used[index] = true;
            current.push(pool[index].clone());
            walk(pool, size, used, current, results);
            current.pop();
            used[index] = false;
        }
    }
    walk(pool, size, &mut used, &mut current, &mut results);
    results
}

pub fn itertools() -> Vec<(&'static str, Value)> {
    vec![
        (
            "product",
            function("product", |interpreter, mut args| {
                let repeat = args
                    .take_keyword("repeat")
                    .and_then(|value| value.as_int())
                    .unwrap_or(1)
                    .max(0) as usize;
                let pools = lists(interpreter, &args.positional)?;
                let mut repeated = Vec::new();
                for _ in 0..repeat {
                    repeated.extend(pools.iter().cloned());
                }
                Ok(Value::iterator(product(&repeated)))
            }),
        ),
        ("chain", Value::native(Chain)),
        (
            "combinations",
            function("combinations", |interpreter, mut args| {
                let size = args
                    .take(1, "r")
                    .and_then(|value| value.as_int())
                    .unwrap_or(0)
                    .max(0) as usize;
                let pool = interpreter.iterate(&args.positional[0])?;
                Ok(Value::iterator(combinations(&pool, size, false)))
            }),
        ),
        (
            "combinations_with_replacement",
            function("combinations_with_replacement", |interpreter, mut args| {
                let size = args
                    .take(1, "r")
                    .and_then(|value| value.as_int())
                    .unwrap_or(0)
                    .max(0) as usize;
                let pool = interpreter.iterate(&args.positional[0])?;
                Ok(Value::iterator(combinations(&pool, size, true)))
            }),
        ),
        (
            "permutations",
            function("permutations", |interpreter, mut args| {
                let size = args.take(1, "r").and_then(|value| value.as_int());
                let pool = interpreter.iterate(&args.positional[0])?;
                let size = size.map_or(pool.len(), |size| size.max(0) as usize);
                Ok(Value::iterator(permutations(&pool, size)))
            }),
        ),
        (
            "groupby",
            function("groupby", |interpreter, mut args| {
                let key = args.take(1, "key").filter(|key| !key.is_none());
                let items = interpreter.iterate(&args.positional[0])?;
                let mut groups: Vec<(Value, Vec<Value>)> = Vec::new();
                for item in items {
                    let measure = match &key {
                        Some(key) => interpreter.call(key, Args::new(vec![item.clone()]))?,
                        None => item.clone(),
                    };
                    let same = match groups.last() {
                        Some((last, _)) => interpreter.equals(last, &measure)?,
                        None => false,
                    };
                    if same {
                        groups.last_mut().expect("a group").1.push(item);
                    } else {
                        groups.push((measure, vec![item]));
                    }
                }
                Ok(Value::iterator(groups.into_iter().map(|(key, items)| {
                    Value::tuple(vec![key, Value::iterator(items)])
                })))
            }),
        ),
        (
            "islice",
            function("islice", |interpreter, args| {
                args.expect("islice", 2, 4)?;
                let number = |value: Option<&Value>| value.and_then(Value::as_int);
                let (start, stop, step) = match args.positional.len() {
                    2 => (0, number(args.positional.get(1)), 1),
                    _ => (
                        number(args.positional.get(1)).unwrap_or(0),
                        number(args.positional.get(2)),
                        number(args.positional.get(3)).unwrap_or(1).max(1),
                    ),
                };
                let iterable = &args.positional[0];
                let items: Vec<Value> = match iterable.plain() {
                    Value::Iterator(state) => {
                        let mut taken = Vec::new();
                        let limit = stop.unwrap_or(i64::MAX);
                        let mut index = 0i64;
                        while index < limit {
                            match state.borrow_mut().next_value() {
                                Some(item) => taken.push(item),
                                None => break,
                            }
                            index += 1;
                        }
                        taken
                    }
                    other => interpreter.iterate(other)?,
                };
                let stop = stop.map_or(items.len(), |stop| (stop.max(0) as usize).min(items.len()));
                let picked: Vec<Value> = items
                    .into_iter()
                    .take(stop)
                    .skip(start.max(0) as usize)
                    .step_by(step as usize)
                    .collect();
                Ok(Value::iterator(picked))
            }),
        ),
        (
            "repeat",
            function("repeat", |_, mut args| {
                let times = args.take(1, "times").and_then(|value| value.as_int());
                let Some(times) = times else {
                    return Err(Error::unsupported("itertools.repeat without a count"));
                };
                let item = args.positional.first().cloned().unwrap_or(Value::None);
                Ok(Value::iterator(std::iter::repeat_n(
                    item,
                    times.max(0) as usize,
                )))
            }),
        ),
        (
            "count",
            function("count", |_, mut args| {
                let start = args.take(0, "start").unwrap_or(Value::Int(0));
                let step = args.take(1, "step").unwrap_or(Value::Int(1));
                let integer =
                    matches!(start.plain(), Value::Int(_)) && matches!(step.plain(), Value::Int(_));
                Ok(Value::Iterator(Rc::new(RefCell::new(Iter::Count {
                    next: crate::number::to_f64(&start).unwrap_or(0.0),
                    step: crate::number::to_f64(&step).unwrap_or(1.0),
                    integer,
                }))))
            }),
        ),
        (
            "zip_longest",
            function("zip_longest", |interpreter, mut args| {
                let fill = args.take_keyword("fillvalue").unwrap_or(Value::None);
                let lists = lists(interpreter, &args.positional)?;
                let count = lists.iter().map(Vec::len).max().unwrap_or(0);
                Ok(Value::iterator((0..count).map(|index| {
                    Value::tuple(
                        lists
                            .iter()
                            .map(|list| list.get(index).cloned().unwrap_or_else(|| fill.clone()))
                            .collect(),
                    )
                })))
            }),
        ),
        (
            "accumulate",
            function("accumulate", |interpreter, mut args| {
                let initial = args.take_keyword("initial");
                let operation = args.take(1, "func").filter(|value| !value.is_none());
                let items = interpreter.iterate(&args.positional[0])?;
                let mut results = Vec::new();
                let mut total = initial.clone();
                if let Some(initial) = initial {
                    results.push(initial);
                }
                for item in items {
                    total = Some(match total {
                        None => item,
                        Some(total) => match &operation {
                            Some(operation) => {
                                interpreter.call(operation, Args::new(vec![total, item]))?
                            }
                            None => interpreter.binary(
                                crate::interpreter::Operator::Add,
                                &total,
                                &item,
                            )?,
                        },
                    });
                    results.push(total.clone().unwrap_or(Value::None));
                }
                Ok(Value::iterator(results))
            }),
        ),
        (
            "starmap",
            function("starmap", |interpreter, args| {
                args.expect("starmap", 2, 2)?;
                let mut results = Vec::new();
                for item in interpreter.iterate(&args.positional[1])? {
                    let call_args = interpreter.iterate(&item)?;
                    results.push(interpreter.call(&args.positional[0], Args::new(call_args))?);
                }
                Ok(Value::iterator(results))
            }),
        ),
        (
            "compress",
            function("compress", |interpreter, args| {
                args.expect("compress", 2, 2)?;
                let items = interpreter.iterate(&args.positional[0])?;
                let selectors = interpreter.iterate(&args.positional[1])?;
                let mut kept = Vec::new();
                for (item, selector) in items.into_iter().zip(selectors) {
                    if interpreter.truthy(&selector)? {
                        kept.push(item);
                    }
                }
                Ok(Value::iterator(kept))
            }),
        ),
        (
            "filterfalse",
            function("filterfalse", |interpreter, args| {
                args.expect("filterfalse", 2, 2)?;
                let mut kept = Vec::new();
                for item in interpreter.iterate(&args.positional[1])? {
                    let truth = if args.positional[0].is_none() {
                        interpreter.truthy(&item)?
                    } else {
                        let result =
                            interpreter.call(&args.positional[0], Args::new(vec![item.clone()]))?;
                        interpreter.truthy(&result)?
                    };
                    if !truth {
                        kept.push(item);
                    }
                }
                Ok(Value::iterator(kept))
            }),
        ),
        (
            "takewhile",
            function("takewhile", |interpreter, args| {
                args.expect("takewhile", 2, 2)?;
                let mut kept = Vec::new();
                for item in interpreter.iterate(&args.positional[1])? {
                    let result =
                        interpreter.call(&args.positional[0], Args::new(vec![item.clone()]))?;
                    if !interpreter.truthy(&result)? {
                        break;
                    }
                    kept.push(item);
                }
                Ok(Value::iterator(kept))
            }),
        ),
        (
            "dropwhile",
            function("dropwhile", |interpreter, args| {
                args.expect("dropwhile", 2, 2)?;
                let mut kept = Vec::new();
                let mut dropping = true;
                for item in interpreter.iterate(&args.positional[1])? {
                    if dropping {
                        let result =
                            interpreter.call(&args.positional[0], Args::new(vec![item.clone()]))?;
                        if interpreter.truthy(&result)? {
                            continue;
                        }
                        dropping = false;
                    }
                    kept.push(item);
                }
                Ok(Value::iterator(kept))
            }),
        ),
        (
            "pairwise",
            function("pairwise", |interpreter, args| {
                args.expect("pairwise", 1, 1)?;
                let items = interpreter.iterate(&args.positional[0])?;
                Ok(Value::iterator(
                    items
                        .windows(2)
                        .map(|pair| Value::tuple(pair.to_vec()))
                        .collect::<Vec<_>>(),
                ))
            }),
        ),
        (
            "batched",
            function("batched", |interpreter, args| {
                args.expect("batched", 2, 2)?;
                let size = args.positional[1].as_int().unwrap_or(1).max(1) as usize;
                let items = interpreter.iterate(&args.positional[0])?;
                Ok(Value::iterator(
                    items
                        .chunks(size)
                        .map(|chunk| Value::tuple(chunk.to_vec()))
                        .collect::<Vec<_>>(),
                ))
            }),
        ),
        (
            "cycle",
            function("cycle", |_, _| Err(Error::unsupported("itertools.cycle"))),
        ),
        (
            "tee",
            function("tee", |interpreter, mut args| {
                let count = args
                    .take(1, "n")
                    .and_then(|value| value.as_int())
                    .unwrap_or(2)
                    .max(0) as usize;
                let items = interpreter.iterate(&args.positional[0])?;
                Ok(Value::tuple(
                    (0..count).map(|_| Value::iterator(items.clone())).collect(),
                ))
            }),
        ),
    ]
}

/// `itertools.chain`, with `chain.from_iterable`.
struct Chain;

impl Native for Chain {
    fn type_name(&self) -> &str {
        "type"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn call(
        &self,
        interpreter: &mut Interpreter,
        _: &Value,
        args: Args,
    ) -> Result<Option<Value>, Error> {
        let mut items = Vec::new();
        for iterable in &args.positional {
            items.extend(interpreter.iterate(iterable)?);
        }
        Ok(Some(Value::iterator(items)))
    }

    fn instance_of(&self, class: &str) -> bool {
        class == "callable"
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        _: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        Ok(match name {
            "from_iterable" => Some(function("from_iterable", |interpreter, args| {
                args.expect("from_iterable", 1, 1)?;
                let mut items = Vec::new();
                for iterable in interpreter.iterate(&args.positional[0])? {
                    items.extend(interpreter.iterate(&iterable)?);
                }
                Ok(Value::iterator(items))
            })),
            _ => None,
        })
    }
}

/// `functools.partial`.
pub struct Partial {
    function: Value,
    args: Vec<Value>,
    keywords: Vec<(Rc<str>, Value)>,
}

impl Native for Partial {
    fn type_name(&self) -> &str {
        "functools.partial"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn instance_of(&self, class: &str) -> bool {
        class == "callable"
    }

    fn call(
        &self,
        interpreter: &mut Interpreter,
        _: &Value,
        args: Args,
    ) -> Result<Option<Value>, Error> {
        let mut positional = self.args.clone();
        positional.extend(args.positional);
        let mut keywords = self.keywords.clone();
        for (name, value) in args.keywords {
            keywords.retain(|(existing, _)| *existing != name);
            keywords.push((name, value));
        }
        interpreter
            .call(
                &self.function,
                Args {
                    positional,
                    keywords,
                },
            )
            .map(Some)
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        _: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        Ok(match name {
            "func" => Some(self.function.clone()),
            "args" => Some(Value::tuple(self.args.clone())),
            "keywords" => Some(Value::dict_of(
                self.keywords
                    .iter()
                    .map(|(key, value)| (key.to_string(), value.clone())),
            )),
            _ => None,
        })
    }
}

/// A decorator that may be used bare or with arguments, like `@lru_cache` or `@lru_cache(None)`,
/// and returns the function unchanged.
fn transparent_decorator(interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    let _ = interpreter;
    match args.positional.first() {
        Some(first)
            if matches!(
                first.plain(),
                Value::Function(_) | Value::Builtin(_) | Value::Method(_)
            ) =>
        {
            Ok(first.clone())
        }
        _ => Ok(function("decorator", |_, args| {
            Ok(args.positional.first().cloned().unwrap_or(Value::None))
        })),
    }
}

pub fn functools() -> Vec<(&'static str, Value)> {
    vec![
        (
            "partial",
            function("partial", |_, mut args| {
                if args.positional.is_empty() {
                    return Err(Error::type_error("partial() missing function"));
                }
                let function = args.positional.remove(0);
                Ok(Value::native(Partial {
                    function,
                    args: args.positional,
                    keywords: args.keywords,
                }))
            }),
        ),
        (
            "reduce",
            function("reduce", |interpreter, args| {
                args.expect("reduce", 2, 3)?;
                let mut items = interpreter.iterate(&args.positional[1])?.into_iter();
                let mut total = match args.positional.get(2) {
                    Some(initial) => initial.clone(),
                    None => items.next().ok_or_else(|| {
                        Error::type_error("reduce() of empty iterable with no initial value")
                    })?,
                };
                for item in items {
                    total = interpreter.call(&args.positional[0], Args::new(vec![total, item]))?;
                }
                Ok(total)
            }),
        ),
        ("lru_cache", function("lru_cache", transparent_decorator)),
        ("cache", function("cache", transparent_decorator)),
        (
            "wraps",
            function("wraps", |_, _| {
                Ok(function("wrapper", |_, args| {
                    Ok(args.positional.first().cloned().unwrap_or(Value::None))
                }))
            }),
        ),
        (
            "update_wrapper",
            function("update_wrapper", |_, args| {
                Ok(args.positional.first().cloned().unwrap_or(Value::None))
            }),
        ),
        (
            "total_ordering",
            function("total_ordering", |_, args| {
                Ok(args.positional.first().cloned().unwrap_or(Value::None))
            }),
        ),
        (
            "cached_property",
            function("cached_property", |_, args| {
                let getter = args.positional.first().cloned().unwrap_or(Value::None);
                Ok(Value::native(crate::builtins::Wrapper(getter, "property")))
            }),
        ),
        (
            "cmp_to_key",
            function("cmp_to_key", |_, args| {
                args.expect("cmp_to_key", 1, 1)?;
                let compare = args.positional[0].clone();
                Ok(Value::builtin("K", move |_, args| {
                    Ok(Value::native(CompareKey {
                        compare: compare.clone(),
                        value: args.positional.first().cloned().unwrap_or(Value::None),
                    }))
                }))
            }),
        ),
    ]
}

/// The key object of `functools.cmp_to_key`.
struct CompareKey {
    compare: Value,
    value: Value,
}

impl Native for CompareKey {
    fn type_name(&self) -> &str {
        "functools.KeyWrapper"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn binary(
        &self,
        interpreter: &mut Interpreter,
        operator: crate::interpreter::Operator,
        other: &Value,
        reflected: bool,
    ) -> Result<Option<Value>, Error> {
        let Some(other) = other.as_native::<CompareKey>() else {
            return Ok(None);
        };
        let (a, b) = if reflected {
            (&other.value, &self.value)
        } else {
            (&self.value, &other.value)
        };
        let result = interpreter.call(&self.compare, Args::new(vec![a.clone(), b.clone()]))?;
        let order = result.as_int().unwrap_or(0);
        use crate::interpreter::Operator as O;
        Ok(Some(Value::Bool(match operator {
            O::Lt => order < 0,
            O::LtE => order <= 0,
            O::Gt => order > 0,
            O::GtE => order >= 0,
            O::Eq => order == 0,
            O::NotEq => order != 0,
            _ => return Ok(None),
        })))
    }
}

/// A class made by `collections.namedtuple`.
pub struct NamedTupleClass {
    name: String,
    fields: Vec<String>,
    defaults: Vec<Value>,
}

/// An object of a `namedtuple` class.
pub struct NamedTuple {
    class: Rc<NamedTupleClass>,
    values: Vec<Value>,
}

impl Native for NamedTupleClass {
    fn type_name(&self) -> &str {
        "type"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn instance_of(&self, class: &str) -> bool {
        class == "callable"
    }

    fn check_instance(&self, value: &Value) -> Option<bool> {
        Some(
            value
                .as_native::<NamedTuple>()
                .is_some_and(|tuple| tuple.class.name == self.name),
        )
    }

    fn call(
        &self,
        _: &mut Interpreter,
        this: &Value,
        mut args: Args,
    ) -> Result<Option<Value>, Error> {
        let class = match this.plain() {
            Value::Native(object) => object.clone(),
            _ => return Ok(None),
        };
        let class: Rc<NamedTupleClass> = match class.as_any().downcast_ref::<NamedTupleClass>() {
            Some(found) => Rc::new(NamedTupleClass {
                name: found.name.clone(),
                fields: found.fields.clone(),
                defaults: found.defaults.clone(),
            }),
            None => return Ok(None),
        };
        let mut values = Vec::with_capacity(class.fields.len());
        let first_default = class.fields.len() - class.defaults.len();
        for (index, field) in class.fields.iter().enumerate() {
            let value = match args.take_keyword(field) {
                Some(value) => value,
                None => match args.positional.get(index) {
                    Some(value) => value.clone(),
                    None if index >= first_default => class.defaults[index - first_default].clone(),
                    None => {
                        return Err(Error::type_error(format!(
                            "{}() missing required argument: '{field}'",
                            class.name
                        )));
                    }
                },
            };
            values.push(value);
        }
        Ok(Some(Value::native(NamedTuple { class, values })))
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        _: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        Ok(match name {
            "_fields" => Some(Value::tuple(
                self.fields
                    .iter()
                    .map(|field| Value::str(field.as_str()))
                    .collect(),
            )),
            "__name__" => Some(Value::str(self.name.as_str())),
            _ => None,
        })
    }
}

impl Native for NamedTuple {
    fn type_name(&self) -> &str {
        &self.class.name
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn instance_of(&self, class: &str) -> bool {
        class == "tuple"
    }

    fn iterate(&self, _: &mut Interpreter) -> Result<Option<Vec<Value>>, Error> {
        Ok(Some(self.values.clone()))
    }

    fn length(&self) -> Option<usize> {
        Some(self.values.len())
    }

    fn get_item(
        &self,
        interpreter: &mut Interpreter,
        _: &Value,
        key: &Value,
    ) -> Result<Option<Value>, Error> {
        interpreter
            .get_item(&Value::tuple(self.values.clone()), key)
            .map(Some)
    }

    fn key_equal(&self, other: &dyn Native) -> bool {
        other
            .as_any()
            .downcast_ref::<NamedTuple>()
            .is_some_and(|other| {
                other.values.len() == self.values.len()
                    && other
                        .values
                        .iter()
                        .zip(&self.values)
                        .all(|(a, b)| crate::value::key_equal(a, b))
            })
    }

    fn key_hash(&self, state: &mut dyn std::hash::Hasher) {
        state.write_usize(self.values.len());
    }

    fn binary(
        &self,
        interpreter: &mut Interpreter,
        operator: crate::interpreter::Operator,
        other: &Value,
        reflected: bool,
    ) -> Result<Option<Value>, Error> {
        let mine = Value::tuple(self.values.clone());
        let theirs = match other.as_native::<NamedTuple>() {
            Some(other) => Value::tuple(other.values.clone()),
            None => other.clone(),
        };
        let (a, b) = if reflected {
            (theirs, mine)
        } else {
            (mine, theirs)
        };
        interpreter.binary(operator, &a, &b).map(Some)
    }

    fn repr(&self, interpreter: &mut Interpreter) -> Result<String, Error> {
        let mut parts = Vec::new();
        for (field, value) in self.class.fields.iter().zip(&self.values) {
            parts.push(format!("{field}={}", interpreter.repr(value)?));
        }
        Ok(format!("{}({})", self.class.name, parts.join(", ")))
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        _: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        if let Some(index) = self.class.fields.iter().position(|field| field == name) {
            return Ok(Some(self.values[index].clone()));
        }
        let class = self.class.clone();
        let values = self.values.clone();
        Ok(match name {
            "_fields" => Some(Value::tuple(
                class
                    .fields
                    .iter()
                    .map(|field| Value::str(field.as_str()))
                    .collect(),
            )),
            "_asdict" => Some(Value::builtin("_asdict", move |_, _| {
                Ok(Value::dict_of(
                    class.fields.iter().cloned().zip(values.iter().cloned()),
                ))
            })),
            "_replace" => Some(Value::builtin("_replace", move |_, args| {
                let mut values = values.clone();
                for (name, value) in args.keywords {
                    if let Some(index) = class.fields.iter().position(|field| **field == *name) {
                        values[index] = value;
                    }
                }
                Ok(Value::native(NamedTuple {
                    class: class.clone(),
                    values,
                }))
            })),
            "count" | "index" => {
                let method: Rc<str> = name.into();
                Some(Value::builtin(name, move |interpreter, args| {
                    crate::methods::call(interpreter, &Value::tuple(values.clone()), &method, args)
                }))
            }
            _ => None,
        })
    }
}

fn namedtuple(interpreter: &mut Interpreter, mut args: Args) -> Result<Value, Error> {
    let defaults = args.take_keyword("defaults");
    args.take_keyword("rename");
    args.take_keyword("module");
    let name = args
        .take(0, "typename")
        .and_then(|name| name.as_str().map(str::to_owned))
        .unwrap_or_default();
    let fields = args
        .take(1, "field_names")
        .unwrap_or(Value::list(Vec::new()));
    let fields: Vec<String> = match fields.plain() {
        Value::Str(text) => text
            .replace(',', " ")
            .split_whitespace()
            .map(str::to_owned)
            .collect(),
        other => interpreter
            .iterate(other)?
            .iter()
            .map(|field| interpreter.to_str(field))
            .collect::<Result<_, _>>()?,
    };
    let defaults = match defaults {
        Some(defaults) if !defaults.is_none() => interpreter.iterate(&defaults)?,
        _ => Vec::new(),
    };
    Ok(Value::native(NamedTupleClass {
        name,
        fields,
        defaults,
    }))
}

/// `collections.deque`, as a list with operations at both ends.
struct Deque(RefCell<std::collections::VecDeque<Value>>);

impl Native for Deque {
    fn type_name(&self) -> &str {
        "collections.deque"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn iterate(&self, _: &mut Interpreter) -> Result<Option<Vec<Value>>, Error> {
        Ok(Some(self.0.borrow().iter().cloned().collect()))
    }

    fn length(&self) -> Option<usize> {
        Some(self.0.borrow().len())
    }

    fn get_item(
        &self,
        interpreter: &mut Interpreter,
        _: &Value,
        key: &Value,
    ) -> Result<Option<Value>, Error> {
        let items: Vec<Value> = self.0.borrow().iter().cloned().collect();
        interpreter.get_item(&Value::list(items), key).map(Some)
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        this: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        let this = this.clone();
        let method =
            |name: &'static str,
             body: fn(&Deque, &mut Interpreter, Args) -> Result<Value, Error>| {
                let this = this.clone();
                Some(Value::builtin(name, move |interpreter, args| {
                    let deque = this.as_native::<Deque>().expect("a deque");
                    body(deque, interpreter, args)
                }))
            };
        Ok(match name {
            "append" => method("append", |deque, _, args| {
                deque.0.borrow_mut().push_back(args.positional[0].clone());
                Ok(Value::None)
            }),
            "appendleft" => method("appendleft", |deque, _, args| {
                deque.0.borrow_mut().push_front(args.positional[0].clone());
                Ok(Value::None)
            }),
            "pop" => method("pop", |deque, _, _| {
                deque
                    .0
                    .borrow_mut()
                    .pop_back()
                    .ok_or_else(|| Error::index_error("pop from an empty deque"))
            }),
            "popleft" => method("popleft", |deque, _, _| {
                deque
                    .0
                    .borrow_mut()
                    .pop_front()
                    .ok_or_else(|| Error::index_error("pop from an empty deque"))
            }),
            "extend" => method("extend", |deque, interpreter, args| {
                let items = interpreter.iterate(&args.positional[0])?;
                deque.0.borrow_mut().extend(items);
                Ok(Value::None)
            }),
            "clear" => method("clear", |deque, _, _| {
                deque.0.borrow_mut().clear();
                Ok(Value::None)
            }),
            _ => None,
        })
    }
}

/// An abstract base class of `collections.abc`, for `isinstance`.
struct AbstractClass(&'static str);

impl Native for AbstractClass {
    fn type_name(&self) -> &str {
        "ABCMeta"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn check_instance(&self, value: &Value) -> Option<bool> {
        let plain = value.plain();
        let mapping = matches!(plain, Value::Dict(_))
            || matches!(plain, Value::Native(object) if object.instance_of("dict") || object.instance_of("Mapping"));
        let sequence = matches!(
            plain,
            Value::List(_) | Value::Tuple(_) | Value::Str(_) | Value::Range(_) | Value::Bytes(_)
        ) || matches!(plain, Value::Native(object) if object.instance_of("list") || object.instance_of("tuple"));
        let set = matches!(plain, Value::Set(_) | Value::FrozenSet(_));
        let iterable = mapping
            || sequence
            || set
            || matches!(plain, Value::Iterator(_))
            || matches!(plain, Value::Native(object) if object.length().is_some());
        Some(match self.0 {
            "Mapping" | "MutableMapping" => mapping,
            "Sequence" => sequence,
            "MutableSequence" => matches!(plain, Value::List(_)),
            "Set" | "MutableSet" => set,
            "Iterable" | "Collection" | "Container" | "Sized" | "Reversible" => iterable,
            "Iterator" | "Generator" => matches!(plain, Value::Iterator(_)),
            "Hashable" => crate::value::Key::new(plain.clone()).is_ok(),
            "Callable" => matches!(
                plain,
                Value::Function(_) | Value::Builtin(_) | Value::Method(_) | Value::Class(_)
            ),
            _ => false,
        })
    }

    fn get_item(
        &self,
        _: &mut Interpreter,
        this: &Value,
        _: &Value,
    ) -> Result<Option<Value>, Error> {
        Ok(Some(this.clone()))
    }
}

pub fn abc_types() -> Vec<(&'static str, Value)> {
    [
        "Mapping",
        "MutableMapping",
        "Sequence",
        "MutableSequence",
        "Set",
        "MutableSet",
        "Iterable",
        "Iterator",
        "Generator",
        "Collection",
        "Container",
        "Sized",
        "Hashable",
        "Callable",
        "Reversible",
    ]
    .into_iter()
    .map(|name| (name, Value::native(AbstractClass(name))))
    .collect()
}

pub fn collections() -> Vec<(&'static str, Value)> {
    let mut entries = vec![
        (
            "OrderedDict",
            Value::Class(types::builtin_class(TypeTag::OrderedDict)),
        ),
        (
            "defaultdict",
            Value::Class(types::builtin_class(TypeTag::DefaultDict)),
        ),
        (
            "Counter",
            Value::Class(types::builtin_class(TypeTag::Counter)),
        ),
        ("namedtuple", function("namedtuple", namedtuple)),
        (
            "deque",
            function("deque", |interpreter, args| {
                let items = match args.positional.first() {
                    Some(iterable) => interpreter.iterate(iterable)?,
                    None => Vec::new(),
                };
                Ok(Value::native(Deque(RefCell::new(
                    items.into_iter().collect(),
                ))))
            }),
        ),
        (
            "ChainMap",
            function("ChainMap", |interpreter, args| {
                // A merged copy: later maps first, as lookups go through the maps in order.
                let mut dict = Dict::default();
                for mapping in args.positional.iter().rev() {
                    for (key, value) in interpreter.mapping_items(mapping)? {
                        dict.entries.insert(Key::new(key)?, value);
                    }
                }
                Ok(Value::dict(dict))
            }),
        ),
    ];
    let abc = super::module("collections.abc", abc_types());
    entries.push(("abc", abc));
    entries
}
