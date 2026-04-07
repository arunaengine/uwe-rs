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
