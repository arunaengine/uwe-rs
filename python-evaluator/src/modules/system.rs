//! Stand-ins for modules about the process and its users: `pwd`, `queue`, `threading`,
//! `multiprocessing` and `pickle`. Code runs in one thread while a workflow is planned, so
//! locks always succeed and queues are plain lists.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::any::Any;
use std::cell::RefCell;
use std::collections::VecDeque;

use super::function;
use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::value::{Args, Native, Value};

/// A `pwd.struct_passwd` for the current user, from the environment.
fn password_entry(name: &str) -> Value {
    let home = std::env::var("HOME").unwrap_or_else(|_| format!("/home/{name}"));
    let fields: Vec<String> = [
        "pw_name",
        "pw_passwd",
        "pw_uid",
        "pw_gid",
        "pw_gecos",
        "pw_dir",
        "pw_shell",
    ]
    .iter()
    .map(|field| (*field).to_owned())
    .collect();
    let values = vec![
        Value::str(name),
        Value::str("x"),
        Value::Int(1000),
        Value::Int(1000),
        Value::str(name),
        Value::str(home),
        Value::str("/bin/bash"),
    ];
    super::collections::named_tuples("struct_passwd", &fields, vec![values])
        .pop()
        .unwrap_or(Value::None)
}

fn user_name() -> String {
    std::env::var("USER").unwrap_or_else(|_| "user".into())
}

pub fn pwd() -> Vec<(&'static str, Value)> {
    vec![
        (
            "getpwuid",
            function("getpwuid", |_, _| Ok(password_entry(&user_name()))),
        ),
        (
            "getpwnam",
            function("getpwnam", |interpreter, args| {
                let name = args.get(0).cloned().unwrap_or(Value::None);
                Ok(password_entry(&interpreter.to_str(&name)?))
            }),
        ),
    ]
}

/// `queue.Queue` and its relatives: items in order, first in first out, or last in first out
/// for a `LifoQueue`.
struct Queue {
    items: RefCell<VecDeque<Value>>,
    last_first: bool,
}

impl Native for Queue {
    fn type_name(&self) -> &str {
        if self.last_first {
            "LifoQueue"
        } else {
            "Queue"
        }
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
        let this = this.clone();
        let method = name.to_owned();
        Ok(Some(match name {
            "put" | "put_nowait" | "get" | "get_nowait" | "empty" | "qsize" | "full"
            | "task_done" | "join" => Value::builtin(name, move |_, args| {
                let Some(queue) = this.as_native::<Queue>() else {
                    return Err(Error::type_error("not a queue"));
                };
                let mut items = queue.items.borrow_mut();
                Ok(match method.as_str() {
                    "put" | "put_nowait" => {
                        items.push_back(args.get(0).cloned().unwrap_or(Value::None));
                        Value::None
                    }
                    "get" | "get_nowait" => {
                        let item = if queue.last_first {
                            items.pop_back()
                        } else {
                            items.pop_front()
                        };
                        match item {
                            Some(item) => item,
                            None => {
                                let class =
                                    crate::types::library_exception("Empty", "queue", "Exception");
                                return Err(Error::from_value(crate::error::exception(
                                    &class,
                                    Vec::new(),
                                )));
                            }
                        }
                    }
                    "empty" => Value::Bool(items.is_empty()),
                    "qsize" => Value::Int(items.len() as i64),
                    "full" => Value::Bool(false),
                    _ => Value::None,
                })
            }),
            _ => return Ok(None),
        }))
    }
}

fn queue_class(name: &'static str, last_first: bool) -> (&'static str, Value) {
    (
        name,
        Value::builtin(name, move |_, _| {
            Ok(Value::native(Queue {
                items: RefCell::new(VecDeque::new()),
                last_first,
            }))
        }),
    )
}

pub fn queue() -> Vec<(&'static str, Value)> {
    vec![
        queue_class("Queue", false),
        queue_class("SimpleQueue", false),
        queue_class("LifoQueue", true),
        (
            "Empty",
            Value::Class(crate::types::library_exception(
                "Empty",
                "queue",
                "Exception",
            )),
        ),
        (
            "Full",
            Value::Class(crate::types::library_exception(
                "Full",
                "queue",
                "Exception",
            )),
        ),
    ]
}

/// A lock. Planning runs in one thread, so acquiring never waits; the lock only remembers
/// whether it is held.
struct Lock(std::cell::Cell<bool>);

impl Native for Lock {
    fn type_name(&self) -> &str {
        "lock"
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
        let this = this.clone();
        let method = name.to_owned();
        Ok(Some(match name {
            "acquire" | "release" | "locked" | "__enter__" | "__exit__" => {
                Value::builtin(name, move |_, _| {
                    let Some(lock) = this.as_native::<Lock>() else {
                        return Err(Error::type_error("not a lock"));
                    };
                    Ok(match method.as_str() {
                        "acquire" => {
                            lock.0.set(true);
                            Value::Bool(true)
                        }
                        "__enter__" => {
                            lock.0.set(true);
                            this.clone()
                        }
                        "locked" => Value::Bool(lock.0.get()),
                        "__exit__" => {
                            lock.0.set(false);
                            Value::Bool(false)
                        }
                        _ => {
                            lock.0.set(false);
                            Value::None
                        }
                    })
                })
            }
            _ => return Ok(None),
        }))
    }
}

fn new_lock(_: &mut Interpreter, _: Args) -> Result<Value, Error> {
    Ok(Value::native(Lock(std::cell::Cell::new(false))))
}

fn cpu_count(_: &mut Interpreter, _: Args) -> Result<Value, Error> {
    Ok(Value::Int(
        std::thread::available_parallelism().map_or(1, |count| count.get()) as i64,
    ))
}

/// A thread. Planning runs in one thread and `time.sleep` does not wait, so `start` runs the
/// target at once, and the thread has ended when `start` returns.
struct Thread {
    target: Value,
    args: Value,
    kwargs: Value,
}

impl Native for Thread {
    fn type_name(&self) -> &str {
        "Thread"
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
        let this = this.clone();
        Ok(Some(match name {
            "start" | "run" => Value::builtin(name, move |interpreter, _| {
                let Some(thread) = this.as_native::<Thread>() else {
                    return Err(Error::type_error("not a thread"));
                };
                if thread.target.is_none() {
                    return Ok(Value::None);
                }
                let mut call = Args::new(interpreter.iterate(&thread.args)?);
                for (key, value) in interpreter.mapping_items(&thread.kwargs)? {
                    call.keywords
                        .push((interpreter.to_str(&key)?.into(), value));
                }
                interpreter.call(&thread.target, call)?;
                Ok(Value::None)
            }),
            "join" => Value::builtin(name, |_, _| Ok(Value::None)),
            "is_alive" => Value::builtin(name, |_, _| Ok(Value::Bool(false))),
            "daemon" => Value::Bool(false),
            "name" => Value::str("Thread-1"),
            _ => return Ok(None),
        }))
    }

    fn set_attribute(&self, _: &mut Interpreter, name: &str, _: Value) -> Result<bool, Error> {
        Ok(matches!(name, "daemon" | "name"))
    }
}

fn new_thread(_: &mut Interpreter, mut args: Args) -> Result<Value, Error> {
    args.take(0, "group");
    Ok(Value::native(Thread {
        target: args.take(1, "target").unwrap_or(Value::None),
        args: args.take(3, "args").unwrap_or(Value::tuple(Vec::new())),
        kwargs: args
            .take(4, "kwargs")
            .filter(|value| !value.is_none())
            .unwrap_or_else(|| Value::dict(Default::default())),
    }))
}

/// Takes the items of a queue made by `queue.Queue`, until a sentinel, for Snakemake's
/// `from_queue`.
pub fn drain_queue(queue: &Value, sentinel: &Value) -> Option<Vec<Value>> {
    let queue = queue.as_native::<Queue>()?;
    let mut items = queue.items.borrow_mut();
    let mut taken = Vec::new();
    while let Some(item) = items.pop_front() {
        if item.is(sentinel) {
            break;
        }
        taken.push(item);
    }
    Some(taken)
}

pub fn threading() -> Vec<(&'static str, Value)> {
    vec![
        ("Thread", function("Thread", new_thread)),
        ("Lock", function("Lock", new_lock)),
        ("RLock", function("RLock", new_lock)),
        (
            "active_count",
            function("active_count", |_, _| Ok(Value::Int(1))),
        ),
        ("get_ident", function("get_ident", |_, _| Ok(Value::Int(1)))),
    ]
}

pub fn multiprocessing() -> Vec<(&'static str, Value)> {
    vec![
        ("cpu_count", function("cpu_count", cpu_count)),
        ("Lock", function("Lock", new_lock)),
        ("RLock", function("RLock", new_lock)),
    ]
}

pub fn pickle() -> Vec<(&'static str, Value)> {
    let refuse = |name: &'static str| {
        (
            name,
            Value::builtin(name, move |_, _| {
                Err(Error::unsupported(format!("pickle.{name}")))
            }),
        )
    };
    vec![
        ("HIGHEST_PROTOCOL", Value::Int(5)),
        ("DEFAULT_PROTOCOL", Value::Int(5)),
        refuse("load"),
        refuse("loads"),
        refuse("dump"),
        refuse("dumps"),
    ]
}

/// The memory of the host from `/proc/meminfo`, in bytes: total and available.
fn host_memory() -> (i64, i64) {
    let text = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let field = |name: &str| {
        text.lines()
            .find(|line| line.starts_with(name))
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|kilobytes| kilobytes.parse::<i64>().ok())
            .map(|kilobytes| kilobytes * 1024)
    };
    let total = field("MemTotal:").unwrap_or(16 << 30);
    (total, field("MemAvailable:").unwrap_or(total))
}

pub fn psutil() -> Vec<(&'static str, Value)> {
    vec![
        (
            "virtual_memory",
            function("virtual_memory", |_, _| {
                let (total, available) = host_memory();
                let fields: Vec<String> = ["total", "available", "percent", "used", "free"]
                    .iter()
                    .map(|field| (*field).to_owned())
                    .collect();
                let used = total - available;
                let values = vec![
                    Value::Int(total),
                    Value::Int(available),
                    Value::Float((used as f64 / total.max(1) as f64 * 1000.0).round() / 10.0),
                    Value::Int(used),
                    Value::Int(available),
                ];
                Ok(
                    super::collections::named_tuples("svmem", &fields, vec![values])
                        .pop()
                        .unwrap_or(Value::None),
                )
            }),
        ),
        ("cpu_count", function("cpu_count", cpu_count)),
    ]
}
