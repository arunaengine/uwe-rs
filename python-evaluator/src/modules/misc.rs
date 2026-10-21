//! Smaller library modules: `sys`, `math`, `datetime`, `time`, `string`, `textwrap`,
//! `logging`, `typing`, `dataclasses`, `subprocess` and others.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;

use super::function;
use crate::builtins::{File, Record};
use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::number::to_f64;
use crate::types;
use crate::value::{Args, Dict, Key, Native, Value};

fn console(name: &str) -> Value {
    Value::native(File {
        path: name.to_owned(),
        mode: "w".into(),
        content: RefCell::new(Vec::new()),
        position: RefCell::new(0),
        written: RefCell::new(Vec::new()),
        closed: RefCell::new(false),
        console: true,
        compressed: false,
    })
}

/// A record whose attributes are also reachable by index, like `sys.version_info`.
struct VersionInfo(Vec<Value>);

impl Native for VersionInfo {
    fn type_name(&self) -> &str {
        "sys.version_info"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn iterate(&self, _: &mut Interpreter) -> Result<Option<Vec<Value>>, Error> {
        Ok(Some(self.0.clone()))
    }

    fn length(&self) -> Option<usize> {
        Some(self.0.len())
    }

    fn get_item(
        &self,
        interpreter: &mut Interpreter,
        _: &Value,
        key: &Value,
    ) -> Result<Option<Value>, Error> {
        interpreter
            .get_item(&Value::tuple(self.0.clone()), key)
            .map(Some)
    }

    fn binary(
        &self,
        interpreter: &mut Interpreter,
        operator: crate::interpreter::Operator,
        other: &Value,
        reflected: bool,
    ) -> Result<Option<Value>, Error> {
        let mine = Value::tuple(self.0.clone());
        let (a, b) = if reflected {
            (other.clone(), mine)
        } else {
            (mine, other.clone())
        };
        interpreter.binary(operator, &a, &b).map(Some)
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        _: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        let index = match name {
            "major" => 0,
            "minor" => 1,
            "micro" => 2,
            "releaselevel" => 3,
            "serial" => 4,
            _ => return Ok(None),
        };
        Ok(self.0.get(index).cloned())
    }
}

pub fn sys(interpreter: &mut Interpreter) -> Vec<(&'static str, Value)> {
    let version = vec![
        Value::Int(3),
        Value::Int(12),
        Value::Int(0),
        Value::str("final"),
        Value::Int(0),
    ];
    let modules = Value::dict_of(
        interpreter
            .modules
            .iter()
            .map(|(name, module)| (name.clone(), module.clone())),
    );
    vec![
        (
            "argv",
            Value::list(
                interpreter
                    .argv
                    .iter()
                    .map(|arg| Value::str(arg.as_str()))
                    .collect(),
            ),
        ),
        ("path", interpreter.sys_path.clone()),
        ("modules", modules),
        ("version", Value::str("3.12.0 (iunctus)")),
        ("version_info", Value::native(VersionInfo(version))),
        ("hexversion", Value::Int(0x030c00f0)),
        ("platform", Value::str("linux")),
        ("byteorder", Value::str("little")),
        ("executable", Value::str("/usr/bin/python3")),
        ("prefix", Value::str("/usr")),
        ("maxsize", Value::Int(i64::MAX)),
        ("stdout", console("<stdout>")),
        ("stderr", console("<stderr>")),
        ("__stdout__", console("<stdout>")),
        ("__stderr__", console("<stderr>")),
        (
            "exit",
            function("exit", |_, args| {
                let class = types::exception_class("SystemExit").expect("exists");
                let code = args.positional.first().cloned().unwrap_or(Value::None);
                Err(Error::from_value(crate::error::exception(
                    &class,
                    vec![code],
                )))
            }),
        ),
        (
            "getrecursionlimit",
            function("getrecursionlimit", |interpreter, _| {
                Ok(Value::Int(interpreter.depth_limit as i64))
            }),
        ),
        (
            "setrecursionlimit",
            function("setrecursionlimit", |_, _| Ok(Value::None)),
        ),
        (
            "getsizeof",
            function("getsizeof", |_, _| Ok(Value::Int(64))),
        ),
        (
            "getdefaultencoding",
            function("getdefaultencoding", |_, _| Ok(Value::str("utf-8"))),
        ),
        (
            "getfilesystemencoding",
            function("getfilesystemencoding", |_, _| Ok(Value::str("utf-8"))),
        ),
        (
            "exc_info",
            function("exc_info", |interpreter, _| {
                Ok(
                    match interpreter
                        .current_exception()
                        .and_then(|error| error.exception().cloned())
                    {
                        Some(exception) => Value::tuple(vec![
                            Value::Class(types::class_of(&exception)),
                            exception,
                            Value::None,
                        ]),
                        None => Value::tuple(vec![Value::None, Value::None, Value::None]),
                    },
                )
            }),
        ),
    ]
}

fn number(args: &Args, index: usize, name: &str) -> Result<f64, Error> {
    args.positional
        .get(index)
        .and_then(to_f64)
        .ok_or_else(|| Error::type_error(format!("{name}() must be a real number")))
}

fn unary_math(name: &'static str, operation: fn(f64) -> f64) -> (&'static str, Value) {
    (
        name,
        Value::builtin(name, move |_, args| {
            args.expect(name, 1, 1)?;
            let value = number(&args, 0, name)?;
            let result = operation(value);
            if result.is_nan() && !value.is_nan() {
                return Err(Error::value_error("math domain error"));
            }
            Ok(Value::Float(result))
        }),
    )
}

pub fn math() -> Vec<(&'static str, Value)> {
    let mut entries = vec![
        ("pi", Value::Float(std::f64::consts::PI)),
        ("e", Value::Float(std::f64::consts::E)),
        ("tau", Value::Float(std::f64::consts::TAU)),
        ("inf", Value::Float(f64::INFINITY)),
        ("nan", Value::Float(f64::NAN)),
        (
            "ceil",
            function("ceil", |interpreter, args| {
                let value = &args.positional[0];
                if let Some(method) = interpreter.lookup_special(value, "__ceil__") {
                    return interpreter.call(&method, Args::default());
                }
                if value.as_int().is_some() || matches!(value.plain(), Value::BigInt(_)) {
                    return Ok(value.plain().clone());
                }
                crate::number::float_to_int(number(&args, 0, "ceil")?.ceil())
            }),
        ),
        (
            "floor",
            function("floor", |_, args| {
                let value = &args.positional[0];
                if value.as_int().is_some() || matches!(value.plain(), Value::BigInt(_)) {
                    return Ok(value.plain().clone());
                }
                crate::number::float_to_int(number(&args, 0, "floor")?.floor())
            }),
        ),
        (
            "trunc",
            function("trunc", |_, args| {
                crate::number::float_to_int(number(&args, 0, "trunc")?.trunc())
            }),
        ),
        (
            "log",
            function("log", |_, args| {
                let value = number(&args, 0, "log")?;
                if value <= 0.0 {
                    return Err(Error::value_error("math domain error"));
                }
                match args.positional.get(1) {
                    Some(_) => Ok(Value::Float(value.ln() / number(&args, 1, "log")?.ln())),
                    None => Ok(Value::Float(value.ln())),
                }
            }),
        ),
        (
            "pow",
            function("pow", |_, args| {
                Ok(Value::Float(
                    number(&args, 0, "pow")?.powf(number(&args, 1, "pow")?),
                ))
            }),
        ),
        (
            "atan2",
            function("atan2", |_, args| {
                Ok(Value::Float(
                    number(&args, 0, "atan2")?.atan2(number(&args, 1, "atan2")?),
                ))
            }),
        ),
        (
            "hypot",
            function("hypot", |_, args| {
                let mut total = 0.0;
                for index in 0..args.positional.len() {
                    let value = number(&args, index, "hypot")?;
                    total += value * value;
                }
                Ok(Value::Float(total.sqrt()))
            }),
        ),
        (
            "copysign",
            function("copysign", |_, args| {
                Ok(Value::Float(
                    number(&args, 0, "copysign")?.copysign(number(&args, 1, "copysign")?),
                ))
            }),
        ),
        (
            "fmod",
            function("fmod", |_, args| {
                Ok(Value::Float(
                    number(&args, 0, "fmod")? % number(&args, 1, "fmod")?,
                ))
            }),
        ),
        (
            "isnan",
            function("isnan", |_, args| {
                Ok(Value::Bool(number(&args, 0, "isnan")?.is_nan()))
            }),
        ),
        (
            "isinf",
            function("isinf", |_, args| {
                Ok(Value::Bool(number(&args, 0, "isinf")?.is_infinite()))
            }),
        ),
        (
            "isfinite",
            function("isfinite", |_, args| {
                Ok(Value::Bool(number(&args, 0, "isfinite")?.is_finite()))
            }),
        ),
        (
            "isclose",
            function("isclose", |_, mut args| {
                let relative = args
                    .take_keyword("rel_tol")
                    .and_then(|value| to_f64(&value))
                    .unwrap_or(1e-9);
                let absolute = args
                    .take_keyword("abs_tol")
                    .and_then(|value| to_f64(&value))
                    .unwrap_or(0.0);
                let (a, b) = (number(&args, 0, "isclose")?, number(&args, 1, "isclose")?);
                Ok(Value::Bool(
                    a == b || (a - b).abs() <= (relative * a.abs().max(b.abs())).max(absolute),
                ))
            }),
        ),
        (
            "gcd",
            function("gcd", |_, args| {
                let mut result: i64 = 0;
                for value in &args.positional {
                    let mut other = value.as_int().unwrap_or(0).abs();
                    let mut current = result;
                    while other != 0 {
                        (current, other) = (other, current % other);
                    }
                    result = current;
                }
                Ok(Value::Int(result))
            }),
        ),
        (
            "isqrt",
            function("isqrt", |_, args| {
                args.expect("isqrt", 1, 1)?;
                args.no_keywords("isqrt")?;
                let value = crate::number::to_bigint(&args.positional[0])
                    .ok_or_else(|| Error::type_error("isqrt() argument must be an integer"))?;
                if value.sign() == num_bigint::Sign::Minus {
                    return Err(Error::value_error("isqrt() argument must be nonnegative"));
                }
                Ok(Value::from_bigint(value.sqrt()))
            }),
        ),
        (
            "factorial",
            function("factorial", |_, args| {
                let value = args.positional[0].as_int().unwrap_or(0);
                if value < 0 {
                    return Err(Error::value_error(
                        "factorial() not defined for negative values",
                    ));
                }
                let mut result = num_bigint::BigInt::from(1);
                for factor in 2..=value {
                    result *= factor;
                }
                Ok(Value::from_bigint(result))
            }),
        ),
        (
            "comb",
            function("comb", |_, args| {
                let n = args.positional[0].as_int().unwrap_or(0);
                let k = args.positional[1].as_int().unwrap_or(0);
                if k < 0 || k > n {
                    return Ok(Value::Int(0));
                }
                let mut result = num_bigint::BigInt::from(1);
                for index in 0..k.min(n - k) {
                    result = result * (n - index) / (index + 1);
                }
                Ok(Value::from_bigint(result))
            }),
        ),
        (
            "prod",
            function("prod", |interpreter, mut args| {
                let mut total = args.take_keyword("start").unwrap_or(Value::Int(1));
                for item in interpreter.iterate(&args.positional[0])? {
                    total = interpreter.binary(crate::interpreter::Operator::Mul, &total, &item)?;
                }
                Ok(total)
            }),
        ),
        (
            "fsum",
            function("fsum", |interpreter, args| {
                let mut total = 0.0;
                for item in interpreter.iterate(&args.positional[0])? {
                    total += to_f64(&item).unwrap_or(0.0);
                }
                Ok(Value::Float(total))
            }),
        ),
        (
            "modf",
            function("modf", |_, args| {
                let value = number(&args, 0, "modf")?;
                Ok(Value::tuple(vec![
                    Value::Float(value.fract()),
                    Value::Float(value.trunc()),
                ]))
            }),
        ),
    ];
    entries.extend([
        unary_math("sqrt", f64::sqrt),
        unary_math("exp", f64::exp),
        unary_math("log2", f64::log2),
        unary_math("log10", f64::log10),
        unary_math("log1p", f64::ln_1p),
        unary_math("fabs", f64::abs),
        unary_math("sin", f64::sin),
        unary_math("cos", f64::cos),
        unary_math("tan", f64::tan),
        unary_math("asin", f64::asin),
        unary_math("acos", f64::acos),
        unary_math("atan", f64::atan),
        unary_math("degrees", f64::to_degrees),
        unary_math("radians", f64::to_radians),
    ]);
    entries
}

pub fn string() -> Vec<(&'static str, Value)> {
    vec![
        (
            "ascii_letters",
            Value::str("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ"),
        ),
        ("ascii_lowercase", Value::str("abcdefghijklmnopqrstuvwxyz")),
        ("ascii_uppercase", Value::str("ABCDEFGHIJKLMNOPQRSTUVWXYZ")),
        ("digits", Value::str("0123456789")),
        ("hexdigits", Value::str("0123456789abcdefABCDEF")),
        ("octdigits", Value::str("01234567")),
        (
            "punctuation",
            Value::str("!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~"),
        ),
        ("whitespace", Value::str(" \t\n\r\x0b\x0c")),
        (
            "printable",
            Value::str(
                "0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~ \t\n\r\x0b\x0c",
            ),
        ),
        (
            "capwords",
            function("capwords", |interpreter, args| {
                let text = args
                    .positional
                    .first()
                    .and_then(|text| text.as_str().map(str::to_owned))
                    .unwrap_or_default();
                let words: Vec<String> = text
                    .split_whitespace()
                    .map(|word| {
                        let capitalized = crate::methods::call(
                            interpreter,
                            &Value::str(word),
                            "capitalize",
                            Args::default(),
                        );
                        capitalized
                            .ok()
                            .and_then(|value| value.as_str().map(str::to_owned))
                            .unwrap_or_default()
                    })
                    .collect();
                Ok(Value::str(words.join(" ")))
            }),
        ),
        (
            "Formatter",
            function("Formatter", |_, _| {
                Err(Error::unsupported("string.Formatter"))
            }),
        ),
        (
            "Template",
            function("Template", |interpreter, args| {
                let text = interpreter.to_str(&args.get(0).cloned().unwrap_or(Value::None))?;
                Ok(super::template::Template::value(text))
            }),
        ),
        (
            "Formatter",
            function("Formatter", |_, _| {
                Ok(Value::native(super::template::Formatter))
            }),
        ),
    ]
}

/// `textwrap.dedent`.
pub fn dedent(text: &str) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut margin: Option<String> = None;
    for line in &lines {
        if line.trim().is_empty() {
            continue;
        }
        let indent: String = line
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        margin = Some(match margin {
            None => indent,
            Some(current) => current
                .chars()
                .zip(indent.chars())
                .take_while(|(a, b)| a == b)
                .map(|(a, _)| a)
                .collect(),
        });
    }
    let margin = margin.unwrap_or_default();
    lines
        .iter()
        .map(|line| {
            if line.trim().is_empty() {
                ""
            } else {
                line.strip_prefix(margin.as_str()).unwrap_or(line)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if !current.is_empty() && current.len() + 1 + word.len() > width {
            lines.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

pub fn textwrap() -> Vec<(&'static str, Value)> {
    fn width(args: &mut Args) -> usize {
        args.take(1, "width")
            .and_then(|width| width.as_int())
            .unwrap_or(70)
            .max(1) as usize
    }
    vec![
        (
            "dedent",
            function("dedent", |_, args| {
                let text = args
                    .positional
                    .first()
                    .and_then(|text| text.as_str().map(str::to_owned))
                    .unwrap_or_default();
                Ok(Value::str(dedent(&text)))
            }),
        ),
        (
            "indent",
            function("indent", |_, args| {
                let text = args
                    .positional
                    .first()
                    .and_then(|text| text.as_str().map(str::to_owned))
                    .unwrap_or_default();
                let prefix = args
                    .positional
                    .get(1)
                    .and_then(|text| text.as_str().map(str::to_owned))
                    .unwrap_or_default();
                let lines: Vec<String> = text
                    .split_inclusive('\n')
                    .map(|line| {
                        if line.trim().is_empty() {
                            line.to_owned()
                        } else {
                            format!("{prefix}{line}")
                        }
                    })
                    .collect();
                Ok(Value::str(lines.concat()))
            }),
        ),
        (
            "wrap",
            function("wrap", |_, mut args| {
                let width = width(&mut args);
                let text = args
                    .positional
                    .first()
                    .and_then(|text| text.as_str().map(str::to_owned))
                    .unwrap_or_default();
                Ok(Value::list(
                    wrap_words(&text, width)
                        .into_iter()
                        .map(Value::str)
                        .collect(),
                ))
            }),
        ),
        (
            "fill",
            function("fill", |_, mut args| {
                let width = width(&mut args);
                let text = args
                    .positional
                    .first()
                    .and_then(|text| text.as_str().map(str::to_owned))
                    .unwrap_or_default();
                Ok(Value::str(wrap_words(&text, width).join("\n")))
            }),
        ),
        (
            "shorten",
            function("shorten", |_, mut args| {
                let width = width(&mut args);
                let text = args
                    .positional
                    .first()
                    .and_then(|text| text.as_str().map(str::to_owned))
                    .unwrap_or_default();
                let joined = text.split_whitespace().collect::<Vec<_>>().join(" ");
                Ok(Value::str(if joined.len() <= width {
                    joined
                } else {
                    let mut kept = String::new();
                    for word in joined.split(' ') {
                        if kept.len() + word.len() + 6 > width {
                            break;
                        }
                        if !kept.is_empty() {
                            kept.push(' ');
                        }
                        kept.push_str(word);
                    }
                    format!("{kept} [...]")
                }))
            }),
        ),
    ]
}

pub fn warnings() -> Vec<(&'static str, Value)> {
    vec![
        (
            "warn",
            function("warn", |interpreter, args| {
                if let Some(message) = args.positional.first() {
                    let text = interpreter.to_str(message)?;
                    interpreter.host.print(&format!("Warning: {text}\n"));
                }
                Ok(Value::None)
            }),
        ),
        (
            "filterwarnings",
            function("filterwarnings", |_, _| Ok(Value::None)),
        ),
        (
            "simplefilter",
            function("simplefilter", |_, _| Ok(Value::None)),
        ),
        (
            "catch_warnings",
            function("catch_warnings", |_, _| {
                Ok(Value::native(NullContext(Value::None)))
            }),
        ),
    ]
}

/// A context manager that does nothing, like `contextlib.nullcontext`.
pub struct NullContext(pub Value);

impl Native for NullContext {
    fn type_name(&self) -> &str {
        "nullcontext"
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
        let value = self.0.clone();
        Ok(match name {
            "__enter__" => Some(Value::builtin("__enter__", move |_, _| Ok(value.clone()))),
            "__exit__" => Some(Value::builtin("__exit__", |_, _| Ok(Value::Bool(false)))),
            _ => None,
        })
    }
}

/// A `logging.Logger` that writes to the host's output.
struct Logger(String);

impl Native for Logger {
    fn type_name(&self) -> &str {
        "logging.Logger"
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
        Ok(match name {
            "debug" | "info" | "warning" | "warn" | "error" | "critical" | "exception" | "log" => {
                let level = name.to_uppercase();
                let skip_level = name == "log";
                Some(Value::builtin(name, move |interpreter, args| {
                    let mut positional = args.positional.clone();
                    if skip_level && !positional.is_empty() {
                        positional.remove(0);
                    }
                    if let Some(message) = positional.first() {
                        let mut text = interpreter.to_str(message)?;
                        if positional.len() > 1 {
                            text = crate::format::percent_format(
                                interpreter,
                                &text,
                                &Value::tuple(positional[1..].to_vec()),
                            )?;
                        }
                        if level != "DEBUG" {
                            interpreter.host.print(&format!("{text}\n"));
                        }
                    }
                    Ok(Value::None)
                }))
            }
            "name" => Some(Value::str(self.0.as_str())),
            "handlers" => Some(Value::list(Vec::new())),
            "level" => Some(Value::Int(20)),
            "setLevel" | "addHandler" | "removeHandler" | "addFilter" | "propagate"
            | "disabled" => Some(Value::builtin(name, |_, _| Ok(Value::None))),
            "isEnabledFor" => Some(Value::builtin(name, |_, _| Ok(Value::Bool(true)))),
            // A name that Python's loggers do not have is an error of the code.
            _ if !matches!(self.0.as_str(), "handler" | "formatter")
                && !LOGGER_ATTRIBUTES.contains(&name) =>
            {
                return Err(Error::attribute_error(format!(
                    "'Logger' object has no attribute '{name}'"
                )));
            }
            _ => None,
        })
    }

    fn set_attribute(&self, _: &mut Interpreter, _: &str, _: Value) -> Result<bool, Error> {
        Ok(true)
    }
}

/// The attributes of Python's `logging.Logger`.
const LOGGER_ATTRIBUTES: &[&str] = &[
    "addFilter",
    "addHandler",
    "callHandlers",
    "critical",
    "debug",
    "disabled",
    "error",
    "exception",
    "fatal",
    "filter",
    "filters",
    "findCaller",
    "getChild",
    "getChildren",
    "getEffectiveLevel",
    "handle",
    "handlers",
    "hasHandlers",
    "info",
    "isEnabledFor",
    "level",
    "log",
    "makeRecord",
    "manager",
    "name",
    "parent",
    "propagate",
    "removeFilter",
    "removeHandler",
    "root",
    "setLevel",
    "warn",
    "warning",
];

pub fn logger(name: &str) -> Value {
    Value::native(Logger(name.to_owned()))
}

pub fn logging() -> Vec<(&'static str, Value)> {
    let root = logger("root");
    let mut entries = vec![
        ("DEBUG", Value::Int(10)),
        ("INFO", Value::Int(20)),
        ("WARNING", Value::Int(30)),
        ("WARN", Value::Int(30)),
        ("ERROR", Value::Int(40)),
        ("CRITICAL", Value::Int(50)),
        (
            "getLogger",
            function("getLogger", |_, args| {
                let name = args
                    .positional
                    .first()
                    .and_then(|name| name.as_str().map(str::to_owned))
                    .unwrap_or_else(|| "root".into());
                Ok(logger(&name))
            }),
        ),
        (
            "basicConfig",
            function("basicConfig", |_, _| Ok(Value::None)),
        ),
        (
            "StreamHandler",
            function("StreamHandler", |_, _| Ok(logger("handler"))),
        ),
        (
            "FileHandler",
            function("FileHandler", |_, _| Ok(logger("handler"))),
        ),
        (
            "Formatter",
            function("Formatter", |_, _| Ok(logger("formatter"))),
        ),
        ("Handler", Value::Class(types::native_class("Handler"))),
        ("Logger", Value::Class(types::native_class("Logger"))),
        (
            "LoggerAdapter",
            Value::Class(types::native_class("LoggerAdapter")),
        ),
    ];
    for level in ["debug", "info", "warning", "error", "critical", "exception"] {
        if let Value::Native(object) = &root
            && let Some(logger) = object.as_any().downcast_ref::<Logger>()
        {
            let name = logger.0.clone();
            let level_name: &'static str = level;
            entries.push((
                level_name,
                Value::builtin(level_name, move |interpreter, args| {
                    let logger = Value::native(Logger(name.clone()));
                    let method = interpreter.get_attribute(&logger, level_name)?;
                    interpreter.call(&method, args)
                }),
            ));
        }
    }
    entries
}

pub fn socket() -> Vec<(&'static str, Value)> {
    vec![
        (
            "gethostname",
            function("gethostname", |_, _| Ok(Value::str("localhost"))),
        ),
        (
            "getfqdn",
            function("getfqdn", |_, _| Ok(Value::str("localhost"))),
        ),
    ]
}

pub fn platform() -> Vec<(&'static str, Value)> {
    vec![
        ("system", function("system", |_, _| Ok(Value::str("Linux")))),
        (
            "machine",
            function("machine", |_, _| Ok(Value::str("x86_64"))),
        ),
        ("node", function("node", |_, _| Ok(Value::str("localhost")))),
        (
            "version",
            function("version", |_, _| Ok(Value::str("#1 SMP"))),
        ),
        (
            "platform",
            function("platform", |_, _| Ok(Value::str("Linux-6.0.0-x86_64"))),
        ),
        (
            "python_version",
            function("python_version", |_, _| Ok(Value::str("3.12.0"))),
        ),
        (
            "release",
            function("release", |_, _| Ok(Value::str("6.0.0"))),
        ),
        (
            "processor",
            function("processor", |_, _| Ok(Value::str("x86_64"))),
        ),
        (
            "python_implementation",
            function("python_implementation", |_, _| Ok(Value::str("CPython"))),
        ),
        // On Linux, the versions of other systems are empty.
        (
            "mac_ver",
            function("mac_ver", |_, _| {
                let empty = Value::str("");
                Ok(Value::tuple(vec![
                    empty.clone(),
                    Value::tuple(vec![empty.clone(), empty.clone(), empty.clone()]),
                    empty,
                ]))
            }),
        ),
        (
            "win32_ver",
            function("win32_ver", |_, _| {
                Ok(Value::tuple(vec![Value::str(""); 4]))
            }),
        ),
    ]
}

pub fn getpass() -> Vec<(&'static str, Value)> {
    vec![(
        "getuser",
        function("getuser", |_, _| {
            Ok(Value::str(
                std::env::var("USER").unwrap_or_else(|_| "user".into()),
            ))
        }),
    )]
}

/// A name of `typing`: subscripting and calling give it back.
pub(crate) struct TypingName(pub(crate) &'static str);

impl Native for TypingName {
    fn type_name(&self) -> &str {
        "typing"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get_item(
        &self,
        _: &mut Interpreter,
        this: &Value,
        _: &Value,
    ) -> Result<Option<Value>, Error> {
        Ok(Some(this.clone()))
    }

    fn binary(
        &self,
        _: &mut Interpreter,
        operator: crate::interpreter::Operator,
        _: &Value,
        _: bool,
    ) -> Result<Option<Value>, Error> {
        Ok(match operator {
            crate::interpreter::Operator::BitOr => Some(Value::native(TypingName(self.0))),
            _ => None,
        })
    }

    fn check_instance(&self, value: &Value) -> Option<bool> {
        let plain = value.plain();
        Some(match self.0 {
            "List" => matches!(plain, Value::List(_)),
            "Dict" | "Mapping" => matches!(plain, Value::Dict(_)),
            "Tuple" => matches!(plain, Value::Tuple(_)),
            "Set" => matches!(plain, Value::Set(_)),
            _ => true,
        })
    }
}

pub fn typing() -> Vec<(&'static str, Value)> {
    let mut entries: Vec<(&'static str, Value)> = [
        "Any",
        "List",
        "Dict",
        "Tuple",
        "Set",
        "FrozenSet",
        "Optional",
        "Union",
        "Callable",
        "Iterable",
        "Iterator",
        "Sequence",
        "Mapping",
        "MutableMapping",
        "Generator",
        "Type",
        "Literal",
        "Final",
        "ClassVar",
        "Annotated",
        "Collection",
        "DefaultDict",
        "OrderedDict",
        "Counter",
        "Deque",
        "Pattern",
        "Match",
        "IO",
        "TextIO",
        "Hashable",
        "Sized",
        "NoReturn",
        "Never",
        "Self",
        "AnyStr",
        "Protocol",
        "Generic",
        "TypeAlias",
        "Awaitable",
        "Coroutine",
    ]
    .into_iter()
    .map(|name| (name, Value::native(TypingName(name))))
    .collect();
    entries.extend([
        ("TYPE_CHECKING", Value::Bool(false)),
        (
            "cast",
            function("cast", |_, args| {
                Ok(args.positional.get(1).cloned().unwrap_or(Value::None))
            }),
        ),
        (
            "TypeVar",
            function("TypeVar", |_, _| Ok(Value::native(TypingName("TypeVar")))),
        ),
        (
            "NewType",
            function("NewType", |_, args| {
                Ok(args.positional.get(1).cloned().unwrap_or(Value::None))
            }),
        ),
        (
            "overload",
            function("overload", |_, args| {
                Ok(args.positional.first().cloned().unwrap_or(Value::None))
            }),
        ),
        (
            "no_type_check",
            function("no_type_check", |_, args| {
                Ok(args.positional.first().cloned().unwrap_or(Value::None))
            }),
        ),
        (
            "get_type_hints",
            function("get_type_hints", |_, _| Ok(Value::dict(Dict::default()))),
        ),
        (
            "NamedTuple",
            function("NamedTuple", |_, _| {
                Err(Error::unsupported("typing.NamedTuple"))
            }),
        ),
        (
            "TypedDict",
            function("TypedDict", |_, _| {
                Err(Error::unsupported("typing.TypedDict"))
            }),
        ),
    ]);
    entries
}

thread_local! {
    static RANDOM: RefCell<u64> = const { RefCell::new(0x2545_f491_4f6c_dd1d) };
}

fn next_random() -> f64 {
    RANDOM.with(|state| {
        let mut value = *state.borrow();
        value ^= value << 13;
        value ^= value >> 7;
        value ^= value << 17;
        *state.borrow_mut() = value;
        (value >> 11) as f64 / (1u64 << 53) as f64
    })
}

pub fn random() -> Vec<(&'static str, Value)> {
    vec![
        (
            "seed",
            function("seed", |_, args| {
                let seed = args.positional.first().and_then(Value::as_int).unwrap_or(0) as u64;
                RANDOM.with(|state| {
                    *state.borrow_mut() = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1
                });
                Ok(Value::None)
            }),
        ),
        (
            "random",
            function("random", |_, _| Ok(Value::Float(next_random()))),
        ),
        (
            "uniform",
            function("uniform", |_, args| {
                let (a, b) = (number(&args, 0, "uniform")?, number(&args, 1, "uniform")?);
                Ok(Value::Float(a + (b - a) * next_random()))
            }),
        ),
        (
            "randint",
            function("randint", |_, args| {
                let a = args.positional[0].as_int().unwrap_or(0);
                let b = args.positional[1].as_int().unwrap_or(0);
                Ok(Value::Int(a + (next_random() * (b - a + 1) as f64) as i64))
            }),
        ),
        (
            "choice",
            function("choice", |interpreter, args| {
                let items = interpreter.iterate(&args.positional[0])?;
                if items.is_empty() {
                    return Err(Error::index_error("Cannot choose from an empty sequence"));
                }
                Ok(items[(next_random() * items.len() as f64) as usize].clone())
            }),
        ),
        (
            "shuffle",
            function("shuffle", |_, args| {
                if let Value::List(items) = args.positional[0].plain() {
                    let mut items = items.borrow_mut();
                    for index in (1..items.len()).rev() {
                        let other = (next_random() * (index + 1) as f64) as usize;
                        items.swap(index, other);
                    }
                }
                Ok(Value::None)
            }),
        ),
        (
            "sample",
            function("sample", |interpreter, mut args| {
                let count = args
                    .take(1, "k")
                    .and_then(|count| count.as_int())
                    .unwrap_or(0)
                    .max(0) as usize;
                let mut items = interpreter.iterate(&args.positional[0])?;
                if count > items.len() {
                    return Err(Error::value_error(
                        "Sample larger than population or is negative",
                    ));
                }
                let mut picked = Vec::new();
                for _ in 0..count {
                    let index = (next_random() * items.len() as f64) as usize;
                    picked.push(items.remove(index));
                }
                Ok(Value::list(picked))
            }),
        ),
    ]
}

/// `io.StringIO`.
struct StringIo(RefCell<String>);

impl Native for StringIo {
    fn type_name(&self) -> &str {
        "_io.StringIO"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn iterate(&self, _: &mut Interpreter) -> Result<Option<Vec<Value>>, Error> {
        let text = self.0.borrow();
        Ok(Some(text.split_inclusive('\n').map(Value::str).collect()))
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        this: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        let this = this.clone();
        Ok(match name {
            "write" => Some(Value::builtin("write", move |interpreter, args| {
                let text = interpreter.to_str(&args.positional[0])?;
                let buffer = this.as_native::<StringIo>().expect("a buffer");
                buffer.0.borrow_mut().push_str(&text);
                Ok(Value::Int(text.chars().count() as i64))
            })),
            "getvalue" | "read" => Some(Value::builtin(name, move |_, _| {
                let buffer = this.as_native::<StringIo>().expect("a buffer");
                let text = buffer.0.borrow().clone();
                Ok(Value::str(text))
            })),
            "readlines" => Some(Value::builtin(name, move |_, _| {
                let buffer = this.as_native::<StringIo>().expect("a buffer");
                let text = buffer.0.borrow().clone();
                Ok(Value::list(
                    text.split_inclusive('\n').map(Value::str).collect(),
                ))
            })),
            "seek" | "close" | "flush" | "truncate" => {
                Some(Value::builtin(name, |_, _| Ok(Value::None)))
            }
            "__enter__" => Some(Value::builtin("__enter__", move |_, _| Ok(this.clone()))),
            "__exit__" => Some(Value::builtin("__exit__", |_, _| Ok(Value::Bool(false)))),
            _ => None,
        })
    }
}

pub fn io() -> Vec<(&'static str, Value)> {
    vec![
        (
            "StringIO",
            function("StringIO", |interpreter, args| {
                let initial = match args.positional.first() {
                    Some(value) => interpreter.to_str(value)?,
                    None => String::new(),
                };
                Ok(Value::native(StringIo(RefCell::new(initial))))
            }),
        ),
        (
            "TextIOWrapper",
            Value::Class(types::native_class("TextIOWrapper")),
        ),
        (
            "open",
            function("open", |interpreter, args| {
                let open = interpreter.builtins.get("open").unwrap_or(Value::None);
                interpreter.call(&open, args)
            }),
        ),
    ]
}

/// `dataclasses.field(...)`.
struct FieldSpec {
    default: Option<Value>,
    factory: Option<Value>,
    /// Whether `__eq__` compares the field, and whether `__hash__` takes it: by default
    /// when it compares, as `field(hash=None)`.
    compare: bool,
    hash: Option<bool>,
    /// Whether `__repr__` shows the field.
    repr: bool,
}

impl Native for FieldSpec {
    fn type_name(&self) -> &str {
        "dataclasses.Field"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The options of `@dataclass(...)` that decide `__eq__` and `__hash__`.
#[derive(Clone, Copy)]
struct DataclassOptions {
    eq: bool,
    frozen: bool,
    unsafe_hash: bool,
}

impl DataclassOptions {
    fn read(interpreter: &mut Interpreter, args: &mut Args) -> Result<Self, Error> {
        let mut flag = |name: &str, default: bool| match args.take_keyword(name) {
            Some(value) => interpreter.truthy(&value),
            None => Ok(default),
        };
        Ok(DataclassOptions {
            eq: flag("eq", true)?,
            frozen: flag("frozen", false)?,
            unsafe_hash: flag("unsafe_hash", false)?,
        })
    }
}

fn dataclass_decorate(
    interpreter: &mut Interpreter,
    class: Value,
    options: DataclassOptions,
) -> Result<Value, Error> {
    let Value::Class(class_ref) = class.plain() else {
        return Err(Error::type_error(
            "dataclass() should be applied to a class",
        ));
    };
    let class_ref = class_ref.clone();
    // Fields of base dataclasses come first.
    let mut fields: Vec<(String, Option<Value>, Option<Value>)> = Vec::new();
    // The fields that `__eq__` compares and that `__hash__` takes.
    let mut compared: Vec<(String, bool, bool)> = Vec::new();
    // The fields that `__repr__` shows.
    let mut shown: Vec<String> = Vec::new();
    for ancestor in class_ref.mro.borrow().iter().rev() {
        let dict = ancestor.dict.borrow();
        let Some(Value::Dict(annotations)) = dict.get("__annotations__") else {
            continue;
        };
        for key in annotations.borrow().entries.keys() {
            let Some(name) = key.0.as_str() else { continue };
            let (default, factory, compare, hash, repr) = match dict.get(name) {
                Some(Value::Native(object)) if object.type_name() == "dataclasses.Field" => {
                    let spec = object
                        .as_any()
                        .downcast_ref::<FieldSpec>()
                        .expect("a field");
                    let hash = spec.hash.unwrap_or(spec.compare);
                    let (default, factory) = (spec.default.clone(), spec.factory.clone());
                    (default, factory, spec.compare, hash, spec.repr)
                }
                Some(value) => (Some(value.clone()), None, true, true, true),
                None => (None, None, true, true, true),
            };
            shown.retain(|existing| existing != name);
            if repr {
                shown.push(name.to_owned());
            }
            fields.retain(|(existing, _, _)| existing != name);
            fields.push((name.to_owned(), default, factory));
            compared.retain(|(existing, _, _)| existing != name);
            compared.push((name.to_owned(), compare, hash));
        }
    }
    let names: Vec<Value> = fields
        .iter()
        .map(|(name, _, _)| Value::str(name.as_str()))
        .collect();
    let fields = Rc::new(fields);
    let init_fields = fields.clone();
    let init = Value::builtin("__init__", move |interpreter, mut args| {
        let this = args.positional.remove(0);
        let Value::Instance(instance) = this.plain() else {
            return Err(Error::type_error("__init__ needs an object"));
        };
        for (index, (name, default, factory)) in init_fields.iter().enumerate() {
            let value = match args.take_keyword(name) {
                Some(value) => value,
                None => match args.positional.get(index) {
                    Some(value) => value.clone(),
                    None => match (default, factory) {
                        (Some(default), _) => default.clone(),
                        (None, Some(factory)) => interpreter.call(factory, Args::default())?,
                        (None, None) => {
                            return Err(Error::type_error(format!(
                                "__init__() missing 1 required positional argument: '{name}'"
                            )));
                        }
                    },
                },
            };
            instance
                .dict
                .borrow_mut()
                .insert(name.as_str().into(), value);
        }
        if let Some(post) = interpreter.lookup_special(&this, "__post_init__") {
            interpreter.call(&post, Args::default())?;
        }
        Ok(Value::None)
    });
    let repr = Value::builtin("__repr__", move |interpreter, args| {
        let this = &args.positional[0];
        let mut parts = Vec::new();
        for name in shown.iter() {
            let value = interpreter.get_attribute(this, name)?;
            parts.push(format!("{name}={}", interpreter.repr(&value)?));
        }
        Ok(Value::str(format!(
            "{}({})",
            types::type_name(this),
            parts.join(", ")
        )))
    });
    let eq_fields: Vec<String> = (compared.iter())
        .filter(|(_, compare, _)| *compare)
        .map(|(name, _, _)| name.clone())
        .collect();
    let eq = Value::builtin("__eq__", move |interpreter, args| {
        let (a, b) = (&args.positional[0], &args.positional[1]);
        if !Rc::ptr_eq(&types::class_of(a), &types::class_of(b)) {
            return Ok(Value::NotImplemented);
        }
        for name in eq_fields.iter() {
            let (x, y) = (
                interpreter.get_attribute(a, name)?,
                interpreter.get_attribute(b, name)?,
            );
            if !interpreter.equals(&x, &y)? {
                return Ok(Value::Bool(false));
            }
        }
        Ok(Value::Bool(true))
    });
    let hash_fields: Vec<String> = (compared.iter())
        .filter(|(_, _, hash)| *hash)
        .map(|(name, _, _)| name.clone())
        .collect();
    let hash = Value::builtin("__hash__", move |interpreter, args| {
        let this = &args.positional[0];
        let mut values = Vec::new();
        for name in hash_fields.iter() {
            values.push(interpreter.get_attribute(this, name)?);
        }
        let hash = interpreter.builtins.get("hash").unwrap_or(Value::None);
        interpreter.call(&hash, Args::new(vec![Value::tuple(values)]))
    });
    let mut dict = class_ref.dict.borrow_mut();
    // Whether the class body defines `__hash__` itself.
    let explicit_hash = dict.contains_key("__hash__");
    if !dict.contains_key("__init__") {
        dict.insert("__init__".into(), init);
    }
    if !dict.contains_key("__repr__") {
        dict.insert("__repr__".into(), repr);
    }
    if options.eq && !dict.contains_key("__eq__") {
        dict.insert("__eq__".into(), eq);
    }
    // The `__hash__` table of the dataclasses module.
    if options.unsafe_hash || (options.eq && options.frozen && !explicit_hash) {
        dict.insert("__hash__".into(), hash);
    } else if options.eq && !options.frozen && !explicit_hash {
        dict.insert("__hash__".into(), Value::None);
    }
    dict.insert("__dataclass_fields__".into(), Value::list(names));
    let _ = interpreter;
    Ok(class)
}

pub fn dataclasses() -> Vec<(&'static str, Value)> {
    vec![
        (
            "dataclass",
            function("dataclass", |interpreter, mut args| {
                let options = DataclassOptions::read(interpreter, &mut args)?;
                match args.positional.first() {
                    Some(class @ Value::Class(_)) => {
                        dataclass_decorate(interpreter, class.clone(), options)
                    }
                    _ => Ok(Value::builtin("dataclass", move |interpreter, args| {
                        dataclass_decorate(
                            interpreter,
                            args.positional.first().cloned().unwrap_or(Value::None),
                            options,
                        )
                    })),
                }
            }),
        ),
        (
            "field",
            function("field", |interpreter, mut args| {
                let compare = match args.take_keyword("compare") {
                    Some(value) => interpreter.truthy(&value)?,
                    None => true,
                };
                let hash = match args.take_keyword("hash") {
                    Some(value) if !value.is_none() => Some(interpreter.truthy(&value)?),
                    _ => None,
                };
                let repr = match args.take_keyword("repr") {
                    Some(value) => interpreter.truthy(&value)?,
                    None => true,
                };
                Ok(Value::native(FieldSpec {
                    default: args.take_keyword("default"),
                    factory: args.take_keyword("default_factory"),
                    compare,
                    hash,
                    repr,
                }))
            }),
        ),
        (
            "asdict",
            function("asdict", |interpreter, args| {
                let this = &args.positional[0];
                let class = types::class_of(this);
                let names = class
                    .lookup("__dataclass_fields__")
                    .unwrap_or(Value::list(Vec::new()));
                let mut dict = Dict::default();
                for name in interpreter.iterate(&names)? {
                    let key = name.as_str().unwrap_or_default().to_owned();
                    let value = interpreter.get_attribute(this, &key)?;
                    dict.insert(&key, value);
                }
                Ok(Value::dict(dict))
            }),
        ),
        (
            "fields",
            function("fields", |interpreter, args| {
                let class = match args.positional[0].plain() {
                    Value::Class(class) => class.clone(),
                    other => types::class_of(other),
                };
                let names = class
                    .lookup("__dataclass_fields__")
                    .unwrap_or(Value::list(Vec::new()));
                let mut fields = Vec::new();
                for name in interpreter.iterate(&names)? {
                    let mut record = Dict::default();
                    record.insert("name", name);
                    fields.push(Value::native(Record(RefCell::new(record), "Field")));
                }
                Ok(Value::tuple(fields))
            }),
        ),
        (
            "is_dataclass",
            function("is_dataclass", |_, args| {
                let class = match args.positional[0].plain() {
                    Value::Class(class) => class.clone(),
                    other => types::class_of(other),
                };
                Ok(Value::Bool(class.lookup("__dataclass_fields__").is_some()))
            }),
        ),
    ]
}

pub fn abc() -> Vec<(&'static str, Value)> {
    vec![
        (
            "ABC",
            Value::Class(types::builtin_class(types::TypeTag::Object)),
        ),
        (
            "ABCMeta",
            Value::Class(types::builtin_class(types::TypeTag::Type)),
        ),
        (
            "abstractmethod",
            function("abstractmethod", |_, args| {
                Ok(args.positional.first().cloned().unwrap_or(Value::None))
            }),
        ),
    ]
}

/// `contextlib.suppress`.
struct Suppress(Vec<Value>);

impl Native for Suppress {
    fn type_name(&self) -> &str {
        "contextlib.suppress"
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
        let kinds = self.0.clone();
        Ok(match name {
            "__enter__" => Some(Value::builtin("__enter__", |_, _| Ok(Value::None))),
            "__exit__" => {
                Some(Value::builtin("__exit__", move |_, args| {
                    let Some(Value::Class(raised)) =
                        args.positional.first().map(|value| value.plain().clone())
                    else {
                        return Ok(Value::Bool(false));
                    };
                    Ok(Value::Bool(kinds.iter().any(|kind| matches!(kind.plain(), Value::Class(kind) if raised.is_subclass(kind)))))
                }))
            }
            _ => None,
        })
    }
}

pub fn contextlib() -> Vec<(&'static str, Value)> {
    vec![
        (
            "suppress",
            function("suppress", |_, args| {
                Ok(Value::native(Suppress(args.positional)))
            }),
        ),
        (
            "nullcontext",
            function("nullcontext", |_, args| {
                Ok(Value::native(NullContext(
                    args.positional.first().cloned().unwrap_or(Value::None),
                )))
            }),
        ),
        (
            "contextmanager",
            // Code may define such context managers while it is planned; they need to pause a
            // generator, so using one is not supported.
            function("contextmanager", |_, args| {
                args.expect("contextmanager", 1, 1)?;
                Ok(Value::builtin("contextmanager", |_, _| {
                    Err(Error::unsupported(
                        "using a context manager made with contextlib.contextmanager",
                    ))
                }))
            }),
        ),
        (
            "redirect_stdout",
            function("redirect_stdout", |_, _| {
                Ok(Value::native(NullContext(Value::None)))
            }),
        ),
        (
            "redirect_stderr",
            function("redirect_stderr", |_, _| {
                Ok(Value::native(NullContext(Value::None)))
            }),
        ),
    ]
}

pub fn fnmatch() -> Vec<(&'static str, Value)> {
    fn matches(name: &str, pattern: &str) -> Result<bool, Error> {
        let regex = fancy_regex::Regex::new(&super::os::glob_regex(pattern))
            .map_err(|error| Error::value_error(error.to_string()))?;
        Ok(regex.is_match(name).unwrap_or(false))
    }
    vec![
        (
            "fnmatch",
            function("fnmatch", |interpreter, args| {
                let name = interpreter.to_str(&args.positional[0])?;
                let pattern = interpreter.to_str(&args.positional[1])?;
                Ok(Value::Bool(matches(&name, &pattern)?))
            }),
        ),
        (
            "fnmatchcase",
            function("fnmatchcase", |interpreter, args| {
                let name = interpreter.to_str(&args.positional[0])?;
                let pattern = interpreter.to_str(&args.positional[1])?;
                Ok(Value::Bool(matches(&name, &pattern)?))
            }),
        ),
        (
            "filter",
            function("filter", |interpreter, args| {
                let pattern = interpreter.to_str(&args.positional[1])?;
                let mut kept = Vec::new();
                for name in interpreter.iterate(&args.positional[0])? {
                    let text = interpreter.to_str(&name)?;
                    if matches(&text, &pattern)? {
                        kept.push(name);
                    }
                }
                Ok(Value::list(kept))
            }),
        ),
        (
            "translate",
            function("translate", |interpreter, args| {
                let pattern = interpreter.to_str(&args.positional[0])?;
                Ok(Value::str(super::os::glob_regex(&pattern)))
            }),
        ),
    ]
}

pub fn errno() -> Vec<(&'static str, Value)> {
    vec![
        ("ENOENT", Value::Int(2)),
        ("EEXIST", Value::Int(17)),
        ("EACCES", Value::Int(13)),
        ("EPERM", Value::Int(1)),
        ("ENOTDIR", Value::Int(20)),
        ("EISDIR", Value::Int(21)),
        ("ENOSPC", Value::Int(28)),
    ]
}

pub fn traceback() -> Vec<(&'static str, Value)> {
    vec![
        (
            "format_exc",
            function("format_exc", |interpreter, _| {
                Ok(Value::str(
                    interpreter
                        .current_exception()
                        .map(|error| format!("{error}\n"))
                        .unwrap_or_default(),
                ))
            }),
        ),
        (
            "print_exc",
            function("print_exc", |interpreter, _| {
                if let Some(error) = interpreter.current_exception() {
                    let text = format!("{error}\n");
                    interpreter.host.print(&text);
                }
                Ok(Value::None)
            }),
        ),
    ]
}

pub fn pprint() -> Vec<(&'static str, Value)> {
    vec![
        (
            "pprint",
            function("pprint", |interpreter, args| {
                let text = interpreter.repr(&args.positional[0])?;
                interpreter.host.print(&format!("{text}\n"));
                Ok(Value::None)
            }),
        ),
        (
            "pformat",
            function("pformat", |interpreter, args| {
                Ok(Value::str(interpreter.repr(&args.positional[0])?))
            }),
        ),
    ]
}

pub fn statistics() -> Vec<(&'static str, Value)> {
    fn numbers(interpreter: &mut Interpreter, args: &Args) -> Result<Vec<f64>, Error> {
        let items = interpreter.iterate(&args.positional[0])?;
        let numbers: Vec<f64> = items.iter().filter_map(to_f64).collect();
        if numbers.is_empty() {
            return Err(Error::new("StatisticsError", "no data points"));
        }
        Ok(numbers)
    }
    vec![
        (
            "mean",
            function("mean", |interpreter, args| {
                let numbers = numbers(interpreter, &args)?;
                Ok(Value::Float(
                    numbers.iter().sum::<f64>() / numbers.len() as f64,
                ))
            }),
        ),
        (
            "median",
            function("median", |interpreter, args| {
                let mut numbers = numbers(interpreter, &args)?;
                numbers.sort_by(|a, b| a.total_cmp(b));
                let middle = numbers.len() / 2;
                Ok(Value::Float(if numbers.len() % 2 == 1 {
                    numbers[middle]
                } else {
                    (numbers[middle - 1] + numbers[middle]) / 2.0
                }))
            }),
        ),
    ]
}

pub fn uuid() -> Vec<(&'static str, Value)> {
    vec![(
        "uuid4",
        function("uuid4", |_, _| {
            let high = (next_random() * u32::MAX as f64) as u32;
            Ok(Value::str(format!(
                "{high:08x}-0000-4000-8000-000000000000"
            )))
        }),
    )]
}

/// `shlex.quote`.
pub fn shell_quote(text: &str) -> String {
    if text.is_empty() {
        return "''".into();
    }
    let safe = text
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "@%+=:,./-_".contains(c));
    if safe {
        text.to_owned()
    } else {
        format!("'{}'", text.replace('\'', "'\"'\"'"))
    }
}

pub fn shlex() -> Vec<(&'static str, Value)> {
    vec![
        (
            "quote",
            function("quote", |interpreter, args| {
                let text = interpreter.to_str(&args.positional[0])?;
                Ok(Value::str(shell_quote(&text)))
            }),
        ),
        (
            "join",
            function("join", |interpreter, args| {
                let mut parts = Vec::new();
                for item in interpreter.iterate(&args.positional[0])? {
                    parts.push(shell_quote(&interpreter.to_str(&item)?));
                }
                Ok(Value::str(parts.join(" ")))
            }),
        ),
        (
            "split",
            function("split", |interpreter, args| {
                let text = interpreter.to_str(&args.positional[0])?;
                let mut words = Vec::new();
                let mut current = String::new();
                let mut quote: Option<char> = None;
                let mut started = false;
                let mut chars = text.chars();
                while let Some(c) = chars.next() {
                    match (quote, c) {
                        (Some(open), c) if c == open => quote = None,
                        (Some('"'), '\\') => current.extend(chars.next()),
                        (Some(_), c) => current.push(c),
                        (None, '\'' | '"') => {
                            quote = Some(c);
                            started = true;
                        }
                        (None, '\\') => {
                            current.extend(chars.next());
                            started = true;
                        }
                        (None, c) if c.is_whitespace() => {
                            if started {
                                words.push(Value::str(std::mem::take(&mut current)));
                                started = false;
                            }
                        }
                        (None, c) => {
                            current.push(c);
                            started = true;
                        }
                    }
                }
                if quote.is_some() {
                    return Err(Error::value_error("No closing quotation"));
                }
                if started {
                    words.push(Value::str(current));
                }
                Ok(Value::list(words))
            }),
        ),
    ]
}

/// The words of `keyword.kwlist`.
pub(crate) const KEYWORDS: &[&str] = &[
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue",
    "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import",
    "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while",
    "with", "yield",
];

pub fn keyword() -> Vec<(&'static str, Value)> {
    vec![
        (
            "kwlist",
            Value::list(KEYWORDS.iter().map(|word| Value::str(*word)).collect()),
        ),
        (
            "iskeyword",
            function("iskeyword", |_, args| {
                Ok(Value::Bool(
                    args.positional
                        .first()
                        .and_then(Value::as_str)
                        .is_some_and(|word| KEYWORDS.contains(&word)),
                ))
            }),
        ),
    ]
}

pub fn locale() -> Vec<(&'static str, Value)> {
    vec![
        ("LC_ALL", Value::Int(6)),
        (
            "setlocale",
            function("setlocale", |_, _| Ok(Value::str("C.UTF-8"))),
        ),
        (
            "getpreferredencoding",
            function("getpreferredencoding", |_, _| Ok(Value::str("UTF-8"))),
        ),
    ]
}

/// A feature of `__future__`. With `annotations`, the annotations of functions stay
/// unevaluated.
pub struct Feature(pub &'static str);

impl Native for Feature {
    fn type_name(&self) -> &str {
        "_Feature"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub fn future() -> Vec<(&'static str, Value)> {
    [
        "annotations",
        "print_function",
        "division",
        "absolute_import",
        "unicode_literals",
        "generator_stop",
    ]
    .into_iter()
    .map(|name| (name, Value::native(Feature(name))))
    .collect()
}

pub fn atexit() -> Vec<(&'static str, Value)> {
    vec![
        (
            "register",
            function("register", |_, args| {
                Ok(args.positional.first().cloned().unwrap_or(Value::None))
            }),
        ),
        ("unregister", function("unregister", |_, _| Ok(Value::None))),
    ]
}

pub fn placeholder(_name: &str) -> Vec<(&'static str, Value)> {
    Vec::new()
}

/// GitPython (`git`), which comes with Snakemake: code can import its classes and exceptions;
/// reading a repository is not supported.
pub fn git(name: &str) -> Vec<(&'static str, Value)> {
    let exceptions: Vec<(&'static str, Value)> = [
        ("GitError", "Exception"),
        ("InvalidGitRepositoryError", "GitError"),
        ("NoSuchPathError", "GitError"),
        ("GitCommandError", "GitError"),
        ("GitCommandNotFound", "GitError"),
        ("BadName", "GitError"),
    ]
    .into_iter()
    .map(|(class, base)| {
        (
            class,
            Value::Class(types::library_exception(class, "git.exc", base)),
        )
    })
    .collect();
    if name == "git.exc" {
        return exceptions;
    }
    let mut entries = exceptions;
    entries.push((
        "Repo",
        Value::builtin("Repo", |_, _| {
            Err(Error::unsupported("reading git repositories with git.Repo"))
        }),
    ));
    entries
}

/// The constants of `tkinter` that name sides and directions, which code imports for
/// other purposes; the widgets are not there.
pub fn tkinter() -> Vec<(&'static str, Value)> {
    let texts = [
        ("N", "n"),
        ("S", "s"),
        ("E", "e"),
        ("W", "w"),
        ("NE", "ne"),
        ("NW", "nw"),
        ("SE", "se"),
        ("SW", "sw"),
        ("NS", "ns"),
        ("EW", "ew"),
        ("NSEW", "nsew"),
        ("CENTER", "center"),
        ("END", "end"),
        ("LEFT", "left"),
        ("RIGHT", "right"),
        ("TOP", "top"),
        ("BOTTOM", "bottom"),
        ("BOTH", "both"),
        ("X", "x"),
        ("Y", "y"),
        ("NONE", "none"),
    ];
    let mut entries: Vec<(&'static str, Value)> = texts
        .into_iter()
        .map(|(name, text)| (name, Value::str(text)))
        .collect();
    entries.extend([
        ("YES", Value::Int(1)),
        ("NO", Value::Int(0)),
        ("TRUE", Value::Int(1)),
        ("FALSE", Value::Int(0)),
    ]);
    entries
}

/// The classes of modules that code imports while the workflow is planned but only uses in
/// its jobs: they can be named, and using them is not supported.
pub fn classes_of(name: &str) -> Vec<(&'static str, Value)> {
    let classes: &[&'static str] = match name {
        "configparser" => &[
            "ConfigParser",
            "RawConfigParser",
            "SafeConfigParser",
            "Error",
            "NoSectionError",
            "NoOptionError",
        ],
        "email.message" => &["EmailMessage", "Message"],
        _ => &[],
    };
    classes
        .iter()
        .map(|class| (*class, Value::Class(types::native_class(class))))
        .collect()
}

/// A version number that compares by its numeric parts, like `packaging.version.Version`.
struct Version(String, Vec<i64>);

impl Version {
    fn parse(text: &str) -> Version {
        let parts = text
            .trim_start_matches('v')
            .split(['.', '-', '+'])
            .map_while(|part| {
                let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
                digits.parse().ok()
            })
            .collect();
        Version(text.to_owned(), parts)
    }
}

impl Native for Version {
    fn type_name(&self) -> &str {
        "Version"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn to_str(&self, _: &mut Interpreter) -> Result<Option<String>, Error> {
        Ok(Some(self.0.clone()))
    }

    fn binary(
        &self,
        interpreter: &mut Interpreter,
        operator: crate::interpreter::Operator,
        other: &Value,
        reflected: bool,
    ) -> Result<Option<Value>, Error> {
        let theirs = match other.plain() {
            Value::Str(text) => Version::parse(text).1,
            Value::Native(object) => match object.as_any().downcast_ref::<Version>() {
                Some(version) => version.1.clone(),
                None => return Ok(None),
            },
            _ => return Ok(None),
        };
        let mine = Value::tuple(self.1.iter().map(|part| Value::Int(*part)).collect());
        let theirs = Value::tuple(theirs.into_iter().map(Value::Int).collect());
        let (a, b) = if reflected {
            (theirs, mine)
        } else {
            (mine, theirs)
        };
        interpreter.binary(operator, &a, &b).map(Some)
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        _: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        Ok(match name {
            "major" => self.1.first().map(|part| Value::Int(*part)),
            "minor" => self.1.get(1).map(|part| Value::Int(*part)),
            "micro" => self.1.get(2).map(|part| Value::Int(*part)),
            "release" => Some(Value::tuple(
                self.1.iter().map(|part| Value::Int(*part)).collect(),
            )),
            "base_version" | "public" => Some(Value::str(self.0.as_str())),
            _ => None,
        })
    }
}

pub fn version_module() -> Vec<(&'static str, Value)> {
    fn make(interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
        let text = interpreter.to_str(&args.positional[0])?;
        Ok(Value::native(Version::parse(&text)))
    }
    vec![
        ("Version", function("Version", make)),
        ("parse", function("parse", make)),
        ("LooseVersion", function("LooseVersion", make)),
        ("StrictVersion", function("StrictVersion", make)),
        ("parse_version", function("parse_version", make)),
    ]
}

pub fn importlib_metadata() -> Vec<(&'static str, Value)> {
    vec![
        (
            "version",
            function("version", |interpreter, args| {
                let name = interpreter.to_str(&args.positional[0])?;
                if name == "snakemake" {
                    return Ok(Value::str("9.27.0"));
                }
                Err(Error::from_value(crate::error::exception(
                    &types::library_exception(
                        "PackageNotFoundError",
                        "importlib.metadata",
                        "ModuleNotFoundError",
                    ),
                    vec![Value::str(name)],
                )))
            }),
        ),
        (
            "PackageNotFoundError",
            Value::Class(types::library_exception(
                "PackageNotFoundError",
                "importlib.metadata",
                "ModuleNotFoundError",
            )),
        ),
    ]
}

pub fn inspect() -> Vec<(&'static str, Value)> {
    vec![
        (
            "isfunction",
            function("isfunction", |_, args| {
                Ok(Value::Bool(matches!(
                    args.positional[0].plain(),
                    Value::Function(_)
                )))
            }),
        ),
        (
            "isclass",
            function("isclass", |_, args| {
                Ok(Value::Bool(matches!(
                    args.positional[0].plain(),
                    Value::Class(_)
                )))
            }),
        ),
        (
            "ismodule",
            function("ismodule", |_, args| {
                Ok(Value::Bool(matches!(
                    args.positional[0].plain(),
                    Value::Module(_)
                )))
            }),
        ),
        (
            "signature",
            function("signature", |_, _| {
                Err(Error::unsupported("inspect.signature"))
            }),
        ),
        (
            "getsource",
            function("getsource", |_, _| {
                Err(Error::unsupported("inspect.getsource"))
            }),
        ),
        (
            "currentframe",
            function("currentframe", |_, _| {
                Err(Error::unsupported("inspect.currentframe"))
            }),
        ),
        (
            "stack",
            function("stack", |_, _| Err(Error::unsupported("inspect.stack"))),
        ),
    ]
}

pub fn types_module() -> Vec<(&'static str, Value)> {
    vec![
        (
            "SimpleNamespace",
            function("SimpleNamespace", |_, args| {
                let mut record = Dict::default();
                for (name, value) in args.keywords {
                    record.entries.insert(Key(Value::Str(name)), value);
                }
                Ok(Value::native(Record(
                    RefCell::new(record),
                    "types.SimpleNamespace",
                )))
            }),
        ),
        (
            "FunctionType",
            Value::Class(types::builtin_class(types::TypeTag::Function)),
        ),
        (
            "LambdaType",
            Value::Class(types::builtin_class(types::TypeTag::Function)),
        ),
        (
            "ModuleType",
            Value::Class(types::builtin_class(types::TypeTag::Module)),
        ),
        (
            "GeneratorType",
            Value::Class(types::builtin_class(types::TypeTag::Iterator)),
        ),
        (
            "MethodType",
            Value::Class(types::builtin_class(types::TypeTag::Method)),
        ),
        (
            "BuiltinFunctionType",
            Value::Class(types::builtin_class(types::TypeTag::BuiltinFunction)),
        ),
    ]
}

pub fn unicodedata() -> Vec<(&'static str, Value)> {
    vec![(
        "normalize",
        function("normalize", |_, args| {
            Ok(args.positional.get(1).cloned().unwrap_or(Value::None))
        }),
    )]
}
