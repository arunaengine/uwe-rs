//! The `.str` accessor of a series or an index: text methods applied to each value. Missing
//! values and values that are no text stay missing.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::any::Any;

use super::frame::{DataFrame, FrameData};
use super::index::IndexObject;
use super::labels::Labels;
use super::series::Series;
use super::{is_missing, nan};
use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::value::{Args, Native, Value};

/// The accessor of a series or an index.
pub struct StrAccessor(pub Value);

/// Methods of Python's `str` that pandas applies to each value with the same meaning.
const PLAIN: &[&str] = &[
    "lower",
    "upper",
    "strip",
    "lstrip",
    "rstrip",
    "startswith",
    "endswith",
    "zfill",
    "title",
    "capitalize",
    "swapcase",
    "casefold",
    "isdigit",
    "isnumeric",
    "isalpha",
    "isalnum",
    "isspace",
    "islower",
    "isupper",
    "istitle",
    "isdecimal",
    "find",
    "rfind",
    "center",
    "ljust",
    "rjust",
    "removeprefix",
    "removesuffix",
    "partition",
    "rpartition",
    "index",
    "rindex",
    "encode",
    "normalize",
    "translate",
];

impl StrAccessor {
    /// The values of the series or index.
    fn values(&self) -> Vec<Value> {
        if let Some(series) = self.0.as_native::<Series>() {
            return series.data().values;
        }
        if let Some(index) = self.0.as_native::<IndexObject>() {
            return index.labels.borrow().values.clone();
        }
        Vec::new()
    }

    /// A result of the same kind as the target, with new values.
    fn result(&self, values: Vec<Value>) -> Value {
        if let Some(series) = self.0.as_native::<Series>() {
            return Series::from_data(series.data().with_values(values));
        }
        IndexObject::value(Labels::single(values, Value::None))
    }
}

/// A regular expression from a pattern argument, with `case=False` and `flags`.
fn regex(
    interpreter: &mut Interpreter,
    pattern: &Value,
    args: &mut Args,
) -> Result<std::rc::Rc<crate::modules::Pattern>, Error> {
    let mut flags = args
        .take_keyword("flags")
        .and_then(|value| value.as_int())
        .unwrap_or(0);
    if let Some(case) = args.take_keyword("case")
        && !interpreter.truthy(&case)?
    {
        flags |= 2;
    }
    crate::modules::compile_regex(&interpreter.to_str(pattern)?, flags)
}

fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        if !c.is_alphanumeric() && c != '_' {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

impl StrAccessor {
    fn call(
        &self,
        interpreter: &mut Interpreter,
        name: &str,
        mut args: Args,
    ) -> Result<Value, Error> {
        let values = self.values();
        let mut results = Vec::with_capacity(values.len());
        match name {
            "contains" | "match" | "fullmatch" => {
                let pattern = args.take(0, "pat").unwrap_or(Value::None);
                let literal = match args.take_keyword("regex") {
                    Some(value) => !interpreter.truthy(&value)?,
                    None => false,
                } && name == "contains";
                let fill = args.take_keyword("na");
                let pattern = if literal {
                    Value::str(escape(&interpreter.to_str(&pattern)?))
                } else {
                    pattern
                };
                let regex = regex(interpreter, &pattern, &mut args)?;
                for value in &values {
                    results.push(match value.as_str() {
                        Some(text) => Value::Bool(match name {
                            "contains" => regex.search_regex().is_match(text).unwrap_or(false),
                            "match" => regex.starts(text),
                            _ => regex.fullmatch(text),
                        }),
                        None => fill.clone().unwrap_or_else(nan),
                    });
                }
            }
            "replace" => {
                let pattern = args.take(0, "pat").unwrap_or(Value::None);
                let replacement = args.take(1, "repl").unwrap_or(Value::None);
                let use_regex = match args.take_keyword("regex") {
                    Some(value) => interpreter.truthy(&value)?,
                    None => false,
                };
                let count = args
                    .take_keyword("n")
                    .and_then(|value| value.as_int())
                    .unwrap_or(-1);
                let sub = if use_regex {
                    let module = interpreter.import_module("re")?;
                    Some(interpreter.get_attribute(&module, "sub")?)
                } else {
                    None
                };
                for value in &values {
                    if value.as_str().is_none() {
                        results.push(nan());
                        continue;
                    }
                    results.push(match &sub {
                        Some(sub) => {
                            let mut call = Args::new(vec![
                                pattern.clone(),
                                replacement.clone(),
                                value.clone(),
                            ]);
                            if count > 0 {
                                call.keywords.push(("count".into(), Value::Int(count)));
                            }
                            interpreter.call(sub, call)?
                        }
                        None => {
                            let method = interpreter.get_attribute(value, "replace")?;
                            interpreter.call(
                                &method,
                                Args::new(vec![
                                    pattern.clone(),
                                    replacement.clone(),
                                    Value::Int(count),
                                ]),
                            )?
                        }
                    });
                }
            }
            "split" | "rsplit" => {
                let pattern = args.take(0, "pat").unwrap_or(Value::None);
                let count = args.take(1, "n").unwrap_or(Value::Int(-1));
                let expand = match args.take_keyword("expand") {
                    Some(value) => interpreter.truthy(&value)?,
                    None => false,
                };
                args.take_keyword("regex");
                for value in &values {
                    if value.as_str().is_none() {
                        results.push(nan());
                        continue;
                    }
                    let method = interpreter.get_attribute(value, name)?;
                    let count = if count.as_int().unwrap_or(-1) <= 0 {
                        Value::Int(-1)
                    } else {
                        count.clone()
                    };
                    results
                        .push(interpreter.call(&method, Args::new(vec![pattern.clone(), count]))?);
                }
                if expand {
                    return self.expand(interpreter, results);
                }
            }
            "len" => {
                for value in &values {
                    results.push(if is_missing(value) {
                        nan()
                    } else {
                        Value::Int(interpreter.length(value)? as i64)
                    });
                }
            }
            "get" => {
                let key = args.take(0, "i").unwrap_or(Value::Int(0));
                for value in &values {
                    results.push(if is_missing(value) {
                        nan()
                    } else {
                        interpreter.get_item(value, &key).unwrap_or_else(|_| nan())
                    });
                }
            }
            "slice" => {
                let start = args.take(0, "start").unwrap_or(Value::None);
                let stop = args.take(1, "stop").unwrap_or(Value::None);
                let step = args.take(2, "step").unwrap_or(Value::None);
                let slice = Value::Slice(std::rc::Rc::new([start, stop, step]));
                for value in &values {
                    results.push(if value.as_str().is_some() {
                        interpreter.get_item(value, &slice)?
                    } else {
                        nan()
                    });
                }
            }
            "join" => {
                let separator = args.take(0, "sep").unwrap_or(Value::str(""));
                let join = interpreter.get_attribute(&separator, "join")?;
                for value in &values {
                    results.push(if is_missing(value) {
                        nan()
                    } else {
                        interpreter.call(&join, Args::new(vec![value.clone()]))?
                    });
                }
            }
            "cat" => {
                let separator = args.take_keyword("sep").unwrap_or(Value::str(""));
                let separator = interpreter.to_str(&separator)?;
                let mut texts = Vec::new();
                for value in values.iter().filter(|value| !is_missing(value)) {
                    texts.push(interpreter.to_str(value)?);
                }
                return Ok(Value::str(texts.join(&separator)));
            }
            "extract" => {
                let pattern = args.take(0, "pat").unwrap_or(Value::None);
                let regex = regex(interpreter, &pattern, &mut args)?;
                for value in &values {
                    let captured =
                        value.as_str().and_then(|text| {
                            regex.search_regex().captures(text).ok().flatten().and_then(
                                |captures| captures.get(1).map(|group| Value::str(group.as_str())),
                            )
                        });
                    results.push(captured.unwrap_or_else(nan));
                }
            }
            "count" => {
                let pattern = args.take(0, "pat").unwrap_or(Value::None);
                let regex = regex(interpreter, &pattern, &mut args)?;
                for value in &values {
                    results.push(match value.as_str() {
                        Some(text) => Value::Int(regex.search_regex().find_iter(text).flatten().count() as i64),
                        None => nan(),
                    });
                }
            }
            _ if PLAIN.contains(&name) => {
                for value in &values {
                    if value.as_str().is_none() {
                        results.push(nan());
                        continue;
                    }
                    let method = interpreter.get_attribute(value, name)?;
                    results.push(interpreter.call(&method, args.clone())?);
                }
            }
            _ => {
                return Err(Error::unsupported(format!(
                    "the pandas string method {name}"
                )));
            }
        }
        Ok(self.result(results))
    }

    /// `split(expand=True)`: one column per part.
    fn expand(&self, interpreter: &mut Interpreter, parts: Vec<Value>) -> Result<Value, Error> {
        let mut rows = Vec::with_capacity(parts.len());
        for value in &parts {
            rows.push(if is_missing(value) {
                Vec::new()
            } else {
                interpreter.iterate(value)?
            });
        }
        let width = rows.iter().map(Vec::len).max().unwrap_or(0);
        let columns = (0..width)
            .map(|column| {
                rows.iter()
                    .map(|row| row.get(column).cloned().unwrap_or_else(nan))
                    .collect()
            })
            .collect();
        let index = match self.0.as_native::<Series>() {
            Some(series) => series.data().index,
            None => Labels::range(rows.len()),
        };
        Ok(DataFrame::from_data(FrameData::new(
            Labels::range(width),
            columns,
            index,
        )))
    }
}

impl Native for StrAccessor {
    fn type_name(&self) -> &str {
        "StringMethods"
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
        Ok(Some(Value::builtin(name, move |interpreter, args| {
            let Some(accessor) = this.as_native::<StrAccessor>() else {
                return Err(Error::type_error("not a string accessor"));
            };
            accessor.call(interpreter, &method, args)
        })))
    }

    fn get_item(
        &self,
        interpreter: &mut Interpreter,
        _: &Value,
        key: &Value,
    ) -> Result<Option<Value>, Error> {
        let mut results = Vec::new();
        // Like `get`, it also takes items of lists, as from `split`.
        for value in self.values() {
            results.push(if is_missing(&value) {
                nan()
            } else {
                interpreter.get_item(&value, key).unwrap_or_else(|_| nan())
            });
        }
        Ok(Some(self.result(results)))
    }
}
