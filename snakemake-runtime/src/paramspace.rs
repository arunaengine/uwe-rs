//! `snakemake.utils.Paramspace`: a table of parameters whose rows are the instances of a
//! parameter space. File patterns name each value as `column~value`, in folders by default
//! or in the file name for the columns of `filename_params`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::any::Any;

use python_evaluator::pandas::{DataFrame, is_missing, nan};
use python_evaluator::{Args, Dict, Error, Interpreter, Native, Value};

use crate::objects::NamedList;
use crate::workflow::workflow_error;

pub struct Paramspace {
    /// The table with its columns in the order of the patterns.
    dataframe: Value,
    columns: Vec<String>,
    /// The pattern with a `{}` for each column, like `{}/{}_{}`.
    pattern: String,
    param_sep: String,
    single_wildcard: Option<String>,
}

/// The pattern with each `{}` replaced by the next part.
fn fill(pattern: &str, parts: &[String]) -> String {
    let mut parts = parts.iter();
    let mut output = String::new();
    let mut rest = pattern;
    while let Some(position) = rest.find("{}") {
        output.push_str(&rest[..position]);
        output.push_str(parts.next().map_or("", String::as_str));
        rest = &rest[position + 2..];
    }
    output.push_str(rest);
    output
}

fn text_argument(
    interpreter: &mut Interpreter,
    value: Option<Value>,
    default: &str,
) -> Result<String, Error> {
    match value {
        Some(value) if !value.is_none() => interpreter.to_str(&value),
        _ => Ok(default.to_owned()),
    }
}

/// `Paramspace(dataframe, filename_params=None, param_sep="~", filename_sep="_",
/// single_wildcard=None)`.
pub fn new(interpreter: &mut Interpreter, mut args: Args) -> Result<Value, Error> {
    let frame = args.take(0, "dataframe").ok_or_else(|| {
        Error::type_error("Paramspace() missing 1 required argument: 'dataframe'")
    })?;
    let Some(table) = frame.as_native::<DataFrame>() else {
        return Err(Error::type_error("Paramspace() needs a pandas DataFrame"));
    };
    let data = table.data();
    let filename_params = args.take(1, "filename_params");
    let param_sep = text_argument(interpreter, args.take(2, "param_sep"), "~")?;
    let filename_sep = text_argument(interpreter, args.take(3, "filename_sep"), "_")?;
    let single_wildcard = match args.take(4, "single_wildcard") {
        Some(value) if !value.is_none() => Some(interpreter.to_str(&value)?),
        _ => None,
    };
    let mut all = Vec::new();
    for label in &data.columns.values {
        all.push(interpreter.to_str(label)?);
    }
    let in_file_name: Vec<String> = match filename_params {
        Some(value) if value.as_str() == Some("*") => all.clone(),
        Some(value) if !value.is_none() => {
            let mut names = Vec::new();
            for item in interpreter.iterate(&value)? {
                names.push(interpreter.to_str(&item)?);
            }
            names
        }
        _ => Vec::new(),
    };
    let (pattern, columns) = if in_file_name.is_empty() {
        (vec!["{}"; all.len()].join("/"), all.clone())
    } else {
        if in_file_name.iter().any(|name| !all.contains(name)) {
            return Err(Error::new(
                "KeyError",
                "One or more entries of filename_params are not valid column names for the param file.",
            ));
        }
        let mut unique = in_file_name.clone();
        unique.sort();
        unique.dedup();
        if unique.len() != in_file_name.len() {
            return Err(Error::value_error("filename_params must be unique"));
        }
        let folders = vec!["{}"; all.len() - in_file_name.len() + 1].join("/");
        let mut parts = vec![folders];
        parts.extend(std::iter::repeat_n("{}".to_owned(), in_file_name.len() - 1));
        let mut columns: Vec<String> = all
            .iter()
            .filter(|name| !in_file_name.contains(name))
            .cloned()
            .collect();
        columns.extend(in_file_name);
        (parts.join(&filename_sep), columns)
    };
    let positions: Vec<usize> = columns
        .iter()
        .filter_map(|name| all.iter().position(|column| column == name))
        .collect();
    Ok(Value::native(Paramspace {
        dataframe: DataFrame::from_data(data.take_columns(&positions)),
        columns,
        pattern,
        param_sep,
        single_wildcard,
    }))
}

impl Paramspace {
    fn wildcard_pattern(&self) -> String {
        match &self.single_wildcard {
            Some(name) => format!("{{{name}}}"),
            None => {
                let parts: Vec<String> = self
                    .columns
                    .iter()
                    .map(|column| format!("{column}{}{{{column}}}", self.param_sep))
                    .collect();
                fill(&self.pattern, &parts)
            }
        }
    }

    fn table(&self) -> python_evaluator::pandas::FrameData {
        self.dataframe
            .as_native::<DataFrame>()
            .map(DataFrame::data)
            .expect("a Paramspace holds a DataFrame")
    }

    /// Each row as a pattern like `alpha~1/beta~0.5`; missing values are `NA`.
    fn instance_patterns(&self, interpreter: &mut Interpreter) -> Result<Vec<Value>, Error> {
        let data = self.table();
        let mut rows: Vec<Vec<String>> = vec![Vec::new(); data.rows()];
        for (column, values) in self.columns.iter().zip(&data.data) {
            for (parts, value) in rows.iter_mut().zip(values) {
                let text = if is_missing(value) {
                    "NA".to_owned()
                } else {
                    interpreter.to_str(value)?
                };
                parts.push(format!("{column}{}{text}", self.param_sep));
            }
        }
        let patterns = rows
            .iter()
            .map(|parts| Value::str(fill(&self.pattern, parts)));
        Ok(patterns.collect())
    }

    /// A wildcard value as a value of the type of its column, as `astype` gives it.
    fn convert(&self, column: &str, text: &str) -> Value {
        if text == "NA" {
            return nan();
        }
        let data = self.table();
        let Some(position) = self.columns.iter().position(|name| name == column) else {
            return Value::str(text);
        };
        let present: Vec<&Value> = data.data[position]
            .iter()
            .filter(|value| !is_missing(value))
            .collect();
        let all = |test: fn(&Value) -> bool| !present.is_empty() && present.iter().all(|v| test(v));
        // Numbers are numpy scalars in Snakemake, which scripts may call `.item()` on; the
        // tag keeps their type for the preambles of scripts. `False` stays Python's.
        if all(|value| matches!(value.plain(), Value::Bool(_))) {
            return match !text.is_empty() && text != "False" {
                true => numpy_scalar(Value::Bool(true), "bool_"),
                false => Value::Bool(false),
            };
        }
        if all(|value| matches!(value.plain(), Value::Int(_)))
            && let Ok(number) = text.parse::<i64>()
        {
            return numpy_scalar(Value::Int(number), "int64");
        }
        if all(|value| matches!(value.plain(), Value::Int(_) | Value::Float(_)))
            && let Ok(number) = text.parse::<f64>()
        {
            return numpy_scalar(Value::Float(number), "float64");
        }
        Value::str(text)
    }

    /// The row of the given wildcard values, as a dict by column.
    fn instance(&self, wildcards: &Value) -> Result<Value, Error> {
        let Some(wildcards) = wildcards.as_native::<NamedList>() else {
            return Err(Error::type_error(
                "Paramspace.instance() needs the wildcards",
            ));
        };
        let mut values: Vec<(String, String)> = Vec::new();
        match &self.single_wildcard {
            Some(name) => {
                let Some(value) = wildcards.named(name) else {
                    return Err(workflow_error(format!(
                        "Error processing paramspace: wildcard {name} is not used in rule."
                    )));
                };
                let text = value.as_str().unwrap_or_default().to_owned();
                let parts: Vec<String> = self
                    .columns
                    .iter()
                    .map(|column| format!("{column}{}{{{column}}}", self.param_sep))
                    .collect();
                let pattern = fill(&self.pattern, &parts);
                let regex = fancy_regex::Regex::new(&crate::pattern::regex_text(&pattern))
                    .map_err(|error| Error::value_error(error.to_string()))?;
                let captures = regex.captures(&text).ok().flatten().ok_or_else(|| {
                    workflow_error(format!(
                        "Error processing paramspace: wildcard {name}={text} does not match pattern {pattern}."
                    ))
                })?;
                for column in &self.columns {
                    if let Some(found) = captures.name(column) {
                        values.push((column.clone(), found.as_str().to_owned()));
                    }
                }
            }
            None => {
                for name in wildcards.keys() {
                    if self.columns.contains(&name)
                        && let Some(value) = wildcards.named(&name)
                    {
                        values.push((name, value.as_str().unwrap_or_default().to_owned()));
                    }
                }
            }
        }
        let mut dict = Dict::default();
        for (column, text) in values {
            dict.insert(column.as_str(), self.convert(&column, &text));
        }
        Ok(Value::dict(dict))
    }
}

/// The tag of a value that Snakemake has as a numpy scalar, with the name of its numpy type.
pub const NUMPY_TAG: &str = "numpy";

fn numpy_scalar(value: Value, kind: &str) -> Value {
    value.with_tag(NUMPY_TAG, Value::str(kind))
}

/// A table that an attribute or item of the underlying table gives, as a new Paramspace.
fn wrap(interpreter: &mut Interpreter, value: Value) -> Result<Value, Error> {
    if value.as_native::<DataFrame>().is_some() {
        new(interpreter, Args::new(vec![value]))
    } else {
        Ok(value)
    }
}

impl Native for Paramspace {
    fn type_name(&self) -> &str {
        "Paramspace"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get_attribute(
        &self,
        interpreter: &mut Interpreter,
        this: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        let this = this.clone();
        Ok(Some(match name {
            "wildcard_pattern" => Value::str(self.wildcard_pattern()),
            "instance_patterns" => Value::iterator(self.instance_patterns(interpreter)?),
            "instance" => Value::builtin("instance", move |_, args| {
                let Some(space) = this.as_native::<Paramspace>() else {
                    return Err(Error::type_error("not a Paramspace"));
                };
                space.instance(args.get(0).unwrap_or(&Value::None))
            }),
            "dataframe" => self.dataframe.clone(),
            "ordered_columns" => Value::list(
                self.columns
                    .iter()
                    .map(|name| Value::str(name.as_str()))
                    .collect(),
            ),
            "pattern" => Value::str(self.pattern.as_str()),
            "param_sep" => Value::str(self.param_sep.as_str()),
            "single_wildcard" => self
                .single_wildcard
                .as_deref()
                .map_or(Value::None, Value::str),
            _ => {
                let value = interpreter.get_attribute(&self.dataframe, name)?;
                wrap(interpreter, value)?
            }
        }))
    }

    fn get_item(
        &self,
        interpreter: &mut Interpreter,
        _: &Value,
        key: &Value,
    ) -> Result<Option<Value>, Error> {
        let value = interpreter.get_item(&self.dataframe, key)?;
        wrap(interpreter, value).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns_fill_one_place_per_column() {
        let parts = ["a~1".to_owned(), "b~2".to_owned(), "c~3".to_owned()];
        assert_eq!(fill("{}/{}_{}", &parts), "a~1/b~2_c~3");
        assert_eq!(fill("{}", &parts[..1]), "a~1");
    }
}
