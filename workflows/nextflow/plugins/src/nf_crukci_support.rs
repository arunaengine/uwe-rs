//! nf-crukci-support 1.1.3, in part: the functions for Java task memory, collections, file names
//! and exception logs; the scanner of task logs is not implemented.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

use nextflow_config::Config;
use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::units::{memory_text, parse_memory};
use nextflow_evaluator::value::Value;

use crate::Context;
use crate::catalogue::{Kind, Plugin, State};
use crate::observer::Observer;

pub(crate) const PLUGIN: Plugin = Plugin {
    functions: &[
        "javaMemMB",
        "javaMemoryOptions",
        "logException",
        "makeCollection",
        "safeName",
        "sizeOf",
    ],
    call: Some(call),
    scopes: &["crukci"],
    observer: Some(activate),
    ..Plugin::new("nf-crukci-support", Kind::Observer, State::Partial)
};

const MB: i64 = 1024 * 1024;
/// The smallest Java heap the functions accept.
const HEAP_MIN: i64 = 16 * MB;

/// The scanner writes `.exitcode` files into task folders, which uwe's executors keep to
/// themselves and whose exit status they decide; a config that sets it up is told so once.
fn activate(
    config: &Config,
    _: &std::path::Path,
    _: &crate::catalogue::Environment,
) -> Result<Option<Box<dyn Observer>>, String> {
    let set = ["patterns", "defaultPatterns", "maxLinesToScan"]
        .into_iter()
        .filter(|key| config.get(&["crukci", key]).is_some())
        .collect::<Vec<_>>();
    if !set.is_empty() {
        tracing::warn!(
            "nf-crukci-support: uwe does not scan task logs; crukci.{} has no effect",
            set.join(", crukci.")
        );
    }
    Ok(None)
}

fn call(name: &str, arguments: Vec<Value>, context: &mut Context) -> Result<Value, Error> {
    let first = arguments.into_iter().next().unwrap_or(Value::Null);
    match name {
        "javaMemoryOptions" => java_memory(&first, context.config),
        "javaMemMB" => {
            let options = java_memory(&first, context.config)?;
            let heap = match crate::nf_schema::get(&options, "heap") {
                Some(Value::MemoryUnit(bytes)) => bytes / MB,
                _ => 0,
            };
            Ok(Value::integer(i32::try_from(heap).unwrap_or(i32::MAX)))
        }
        "sizeOf" => {
            let size = match &first {
                Value::Null => 0,
                Value::List(list) => list.borrow().items.len(),
                Value::Set(set) => set.borrow().items.len(),
                Value::Map(map) => map.borrow().entries.len(),
                _ => 1,
            };
            Ok(Value::integer(i32::try_from(size).unwrap_or(i32::MAX)))
        }
        "makeCollection" => Ok(match first {
            Value::Null => Value::Null,
            collection @ (Value::List(_) | Value::Set(_)) => collection,
            other => Value::list(vec![other]),
        }),
        "safeName" => {
            let text = match first {
                Value::Null => String::new(),
                other => other.to_string(),
            };
            Ok(Value::string(safe_name(&text)))
        }
        "logException" => {
            let exception = match first {
                Value::Exception(exception) => exception,
                other => Rc::new(Exception::illegal_argument(other.to_string())),
            };
            tracing::error!("{exception}");
            Err(Error::Thrown {
                exception,
                span: Default::default(),
            })
        }
        _ => Err(Error::Unsupported {
            message: format!("the function `{name}` of the plugin `nf-crukci-support`"),
            span: Default::default(),
        }),
    }
}

/// The heap, metaspace and overhead of a Java task within its `memory`, with its JVM options.
fn java_memory(task: &Value, config: &Config) -> Result<Value, Error> {
    let field = |name: &str| crate::nf_schema::get(task, name);
    let process = field("process")
        .map(|name| name.to_string())
        .unwrap_or_default();
    let name = match field("tag").filter(|tag| !matches!(tag, Value::Null)) {
        Some(tag) => format!("{process} ({tag})"),
        None => process,
    };
    let thrown = |message: String| Error::Thrown {
        exception: Rc::new(Exception::new("IllegalStateException", message)),
        span: Default::default(),
    };
    let Some(Value::MemoryUnit(memory)) = field("memory") else {
        return Err(thrown(format!("Task {name} has no memory set")));
    };
    let overhead = setting(config, "javaOverhead", 64, 32);
    let metaspace = setting(config, "javaMetaspace", 128, 64);
    let heap = (memory - overhead - metaspace).max(0);
    if heap < HEAP_MIN {
        let attempt = field("attempt")
            .map(|attempt| attempt.to_string())
            .unwrap_or_default();
        tracing::error!(
            "Task {name} attempt {attempt}: allocated {}; JVM overhead {}; Java Meta Space {}.",
            memory_text(memory),
            memory_text(overhead),
            memory_text(metaspace)
        );
        return Err(thrown(format!(
            "Insufficent memory for this Java task to run. Task is allocated {}; overheads are \
             {}; minimum size for Java heap is 16 MB. The task needs at least {} allocated.",
            memory_text(memory),
            memory_text(overhead + metaspace),
            memory_text(overhead + metaspace + HEAP_MIN)
        )));
    }
    let options = format!(
        "-XX:MaxMetaspaceSize={}m -Xms{}m -Xmx{}m",
        metaspace / MB,
        heap / MB,
        heap / MB
    );
    let entry = |key: &str, value: Value| (Value::string(key), value);
    Ok(Value::map([
        entry("heap", Value::MemoryUnit(heap)),
        entry("metaSpace", Value::MemoryUnit(metaspace)),
        entry("misc", Value::MemoryUnit(overhead)),
        entry("all", Value::MemoryUnit(memory)),
        entry("jvmOpts", Value::string(options)),
    ]))
}

/// A memory setting of the `crukci` scope in bytes, raised to its minimum with one warning.
fn setting(config: &Config, key: &str, default: i64, least: i64) -> i64 {
    static WARNED: [AtomicBool; 2] = [AtomicBool::new(false), AtomicBool::new(false)];
    let bytes = match config.get(&["crukci", key]) {
        Some(Value::MemoryUnit(bytes)) => bytes,
        Some(Value::Null) | None => default * MB,
        Some(other) => parse_memory(&other.to_string()).unwrap_or(default * MB),
    };
    if bytes >= least * MB {
        return bytes;
    }
    let warned = &WARNED[usize::from(key == "javaMetaspace")];
    if !warned.swap(true, Ordering::Relaxed) {
        tracing::warn!(
            "{key} is set to {}, which is too small. Setting to the minimum of {least} MB.",
            memory_text(bytes)
        );
    }
    least * MB
}

/// ASCII letters, digits, `_`, `-` and `.` stay; spaces and tabs go; the rest becomes `_`.
fn safe_name(text: &str) -> String {
    (text.chars())
        .filter(|character| !matches!(character, ' ' | '\t'))
        .map(|character| match character {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '_' | '-' | '.' => character,
            _ => '_',
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_names() {
        assert_eq!(safe_name("My Sample: Run 1 (2026)"), "MySample_Run1_2026_");
        assert_eq!(safe_name("Data/File.txt"), "Data_File.txt");
        assert_eq!(safe_name("Tab\tRemoved"), "TabRemoved");
        assert_eq!(safe_name("Ünïcode-ß.txt"), "_n_code-_.txt");
    }
}
