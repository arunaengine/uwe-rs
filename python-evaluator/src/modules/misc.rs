//! Smaller library modules: `sys`, `math`, `datetime`, `time`, `string`, `textwrap`,
//! `logging`, `typing`, `dataclasses`, `subprocess` and others.
// Copyright (c) 2026 The iunctus Contributors
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
                let value = args.positional[0].as_int().unwrap_or(0);
                if value < 0 {
                    return Err(Error::value_error("isqrt() argument must be nonnegative"));
                }
                let mut root = (value as f64).sqrt() as i64;
                while root * root > value {
                    root -= 1;
                }
                while (root + 1) * (root + 1) <= value {
                    root += 1;
                }
                Ok(Value::Int(root))
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

/// A point in time for `datetime` and `time`, in local time.
#[derive(Clone)]
struct DateTime {
    time: jiff::Zoned,
    date_only: bool,
}

impl Native for DateTime {
    fn type_name(&self) -> &str {
        if self.date_only {
            "datetime.date"
        } else {
            "datetime.datetime"
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn to_str(&self, _: &mut Interpreter) -> Result<Option<String>, Error> {
        Ok(Some(if self.date_only {
            self.time.strftime("%Y-%m-%d").to_string()
        } else {
            self.time.strftime("%Y-%m-%d %H:%M:%S%.6f").to_string()
        }))
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        _: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        let this = self.clone();
        Ok(Some(match name {
            "year" => Value::Int(i64::from(this.time.year())),
            "month" => Value::Int(i64::from(this.time.month())),
            "day" => Value::Int(i64::from(this.time.day())),
            "hour" => Value::Int(i64::from(this.time.hour())),
            "minute" => Value::Int(i64::from(this.time.minute())),
            "second" => Value::Int(i64::from(this.time.second())),
            "strftime" => Value::builtin("strftime", move |_, args| {
                let format = args
                    .positional
                    .first()
                    .and_then(|format| format.as_str().map(str::to_owned))
                    .unwrap_or_default();
                Ok(Value::str(this.time.strftime(&format).to_string()))
            }),
            "isoformat" => Value::builtin("isoformat", move |_, _| {
                Ok(Value::str(if this.date_only {
                    this.time.strftime("%Y-%m-%d").to_string()
                } else {
                    this.time.strftime("%Y-%m-%dT%H:%M:%S%.6f").to_string()
                }))
            }),
            "date" => Value::builtin("date", move |_, _| {
                Ok(Value::native(DateTime {
                    time: this.time.clone(),
                    date_only: true,
                }))
            }),
            "timestamp" => Value::builtin("timestamp", move |_, _| {
                Ok(Value::Float(this.time.timestamp().as_second() as f64))
            }),
            _ => return Ok(None),
        }))
    }
}

struct DateClass(bool);

impl Native for DateClass {
    fn type_name(&self) -> &str {
        "type"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn check_instance(&self, value: &Value) -> Option<bool> {
        Some(value.as_native::<DateTime>().is_some())
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        _: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        let date_only = self.0;
        Ok(match name {
            "now" | "today" | "utcnow" => Some(Value::builtin(name, move |_, _| {
                Ok(Value::native(DateTime {
                    time: jiff::Zoned::now(),
                    date_only,
                }))
            })),
            "fromtimestamp" => Some(Value::builtin("fromtimestamp", move |_, args| {
                let seconds = args.positional.first().and_then(to_f64).unwrap_or(0.0) as i64;
                let timestamp = jiff::Timestamp::from_second(seconds)
                    .map_err(|error| Error::value_error(error.to_string()))?;
                Ok(Value::native(DateTime {
                    time: timestamp.to_zoned(jiff::tz::TimeZone::system()),
                    date_only,
                }))
            })),
            _ => None,
        })
    }
}

pub fn datetime() -> Vec<(&'static str, Value)> {
    vec![
        ("datetime", Value::native(DateClass(false))),
        ("date", Value::native(DateClass(true))),
        (
            "timedelta",
            function("timedelta", |_, _| {
                Err(Error::unsupported("datetime.timedelta"))
            }),
        ),
    ]
}

pub fn time() -> Vec<(&'static str, Value)> {
    vec![
        (
            "time",
            function("time", |_, _| {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0.0, |duration| duration.as_secs_f64());
                Ok(Value::Float(now))
            }),
        ),
        (
            "perf_counter",
            function("perf_counter", |_, _| Ok(Value::Float(0.0))),
        ),
        (
            "monotonic",
            function("monotonic", |_, _| Ok(Value::Float(0.0))),
        ),
        ("sleep", function("sleep", |_, _| Ok(Value::None))),
        (
            "strftime",
            function("strftime", |_, args| {
                let format = args
                    .positional
                    .first()
                    .and_then(|format| format.as_str().map(str::to_owned))
                    .unwrap_or_default();
                Ok(Value::str(jiff::Zoned::now().strftime(&format).to_string()))
            }),
        ),
        (
            "localtime",
            function("localtime", |_, _| {
                Ok(Value::native(DateTime {
                    time: jiff::Zoned::now(),
                    date_only: false,
                }))
            }),
        ),
    ]
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
            function("Template", |_, _| {
                Err(Error::unsupported("string.Template"))
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

pub fn subprocess() -> Vec<(&'static str, Value)> {
    let refuse = |name: &'static str| {
        (
            name,
            Value::builtin(name, move |_, _| {
                Err(Error::unsupported(format!(
                    "subprocess.{name}: running commands while the workflow is planned"
                )))
            }),
        )
    };
    vec![
        ("PIPE", Value::Int(-1)),
        ("STDOUT", Value::Int(-2)),
        ("DEVNULL", Value::Int(-3)),
        (
            "CalledProcessError",
            Value::Class(types::library_exception(
                "CalledProcessError",
                "subprocess",
                "Exception",
            )),
        ),
        (
            "SubprocessError",
            Value::Class(types::library_exception(
                "SubprocessError",
                "subprocess",
                "Exception",
            )),
        ),
        refuse("run"),
        refuse("check_output"),
        refuse("check_call"),
        refuse("call"),
        refuse("Popen"),
        refuse("getoutput"),
        refuse("getstatusoutput"),
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
