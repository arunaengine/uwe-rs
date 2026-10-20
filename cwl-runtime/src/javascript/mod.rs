//! JavaScript for CWL expressions, run in Boa, a JavaScript engine written in Rust, so that no
//! Node.js is needed.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod budget;
pub(crate) mod helper;
mod image;
mod thread;

use std::future::Future;
use std::pin::pin;
use std::task::{Poll, Waker};
use std::time::{Duration, Instant};

use boa_engine::{Context, JsError, JsValue, Script, Source};

use crate::javascript::helper::Failure;
use crate::value::Value;

const LOOP_LIMIT: u64 = 100_000_000;
const VM_SLICE: u32 = 256;
const VM_SLICES: u64 = 400_000;
const TIME_LIMIT: Duration = Duration::from_secs(20);

/// A console object whose methods do nothing.
const CONSOLE: &str = "var console = {log: function() {}, info: function() {}, \
    warn: function() {}, error: function() {}, debug: function() {}};\n";

/// Marks a result of `undefined`, which JSON cannot hold.
const UNDEFINED: &str = "\u{0}undefined";

/// Runs a function body after the expression library and the global variables, the way
/// cwltool's sandbox does, and returns its result as JSON data.
#[cfg(test)]
pub fn evaluate(
    library: &[String],
    variables: &[(&str, &Value)],
    body: &str,
) -> Result<Value, String> {
    let literals: Vec<_> = variables
        .iter()
        .map(|(name, value)| (*name, value.to_javascript()))
        .collect();
    let variables: Vec<_> = literals
        .iter()
        .map(|(name, value)| (*name, value.as_str()))
        .collect();
    evaluate_literals(library, &variables, body).map_err(Failure::text)
}

pub(crate) fn evaluate_literals(
    library: &[String],
    variables: &[(&str, &str)],
    body: &str,
) -> Result<Value, Failure> {
    let deadline = Instant::now() + TIME_LIMIT;
    // Expressions that log for debugging find a console that drops what they write, as
    // cwltool's does unless `--js-console` shows it.
    let mut code = String::from(CONSOLE);
    for part in library {
        code.push_str(part);
        code.push('\n');
    }
    for (name, value) in variables {
        code.push_str(&format!("var {name} = {value};\n"));
    }
    code.push_str(&format!(
        "var $iunctus = (function(){{{body}\n}})();\n\
         $iunctus === undefined ? {UNDEFINED:?} : JSON.stringify($iunctus)"
    ));
    let mut context = Context::default();
    context
        .runtime_limits_mut()
        .set_loop_iteration_limit(LOOP_LIMIT);
    let result = bounded_eval_typed(&code, &mut context, VM_SLICES, deadline)?;
    let Some(text) = result.as_string() else {
        // JSON.stringify gives undefined for functions and symbols.
        return Ok(Value::Null);
    };
    let text = text.to_std_string_escaped();
    if text == UNDEFINED {
        return Ok(Value::Null);
    }
    crate::javascript::helper::check_frame(&text, crate::javascript::helper::NESTING_LIMIT)
        .map_err(Failure::Boundary)?;
    Value::parse(&text)
        .map_err(|error| Failure::Language(format!("the result is not JSON: {error}")))
}

/// Limits VM work and checks elapsed time between slices. Parsing, compilation and
/// native builtins run synchronously, and allocations have no heap ceiling here.
#[cfg(test)]
fn bounded_eval(
    code: &str,
    context: &mut Context,
    slices: u64,
    deadline: Instant,
) -> Result<JsValue, String> {
    bounded_eval_typed(code, context, slices, deadline).map_err(Failure::text)
}

fn bounded_eval_typed(
    code: &str,
    context: &mut Context,
    slices: u64,
    deadline: Instant,
) -> Result<JsValue, Failure> {
    let script = Script::parse(Source::from_bytes(code.as_bytes()), None, context)
        .map_err(|error| failure(&error, context))?;
    let result = {
        let mut evaluation = pin!(script.evaluate_async_with_budget(context, VM_SLICE));
        let mut task = std::task::Context::from_waker(Waker::noop());
        let mut spent = 0;
        loop {
            if Instant::now() >= deadline {
                return Err(Failure::Boundary(
                    "JavaScript evaluation exceeded its time deadline".into(),
                ));
            }
            if spent >= slices {
                return Err(Failure::Boundary(
                    "JavaScript evaluation exceeded its total VM work budget".into(),
                ));
            }
            match evaluation.as_mut().poll(&mut task) {
                Poll::Ready(result) => {
                    if Instant::now() >= deadline {
                        return Err(Failure::Boundary(
                            "JavaScript evaluation exceeded its time deadline".into(),
                        ));
                    }
                    break result;
                }
                Poll::Pending => spent += 1,
            }
        }
    };
    result.map_err(|error| failure(&error, context))
}

/// An error as `TypeError: message`, without Boa's place in the code that wraps the
/// expression, which means nothing to the author of the expression.
fn failure(error: &JsError, context: &mut Context) -> Failure {
    if let Some(engine) = error.as_engine() {
        return Failure::Boundary(format!("JavaScript engine failure: {engine}"));
    }
    Failure::Language(match error.try_native(context) {
        Ok(native) if native.message().trim().is_empty() => native.kind().to_string(),
        Ok(native) => format!("{}: {}", native.kind(), native.message().trim()),
        Err(_) => error.to_string(),
    })
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
