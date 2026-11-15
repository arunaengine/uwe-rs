//! Tier 1 Galaxy values in templates: params, selects and datasets as python-evaluator objects
//! with Galaxy's printed forms, truth values and comparisons, and the raw values of filters.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::any::Any;
use std::collections::BTreeMap;

use galaxy_parser::tool::{Input as Declared, Param, ParamKind, Tool, at_least, truthy};
use python_evaluator::interpreter::Operator;
use python_evaluator::{Error as Python, Interpreter, Native, Value};
use serde_json::Value as Json;

use crate::Error;
use crate::sanitize::Sanitizer;

/// What one run of a tool gets: raw parameter values, files, and where outputs go.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Job {
    /// Raw values nested like the inputs: sections and conditionals as objects. Missing
    /// values take the defaults of their params.
    pub parameters: Json,
    /// The files of data inputs by path, like `adv|control_sgrna`; a missing path is absent.
    pub datasets: BTreeMap<String, Vec<DataFile>>,
    /// The paths of the outputs by name.
    pub outputs: BTreeMap<String, String>,
    /// The paths where the configfiles are written, by name.
    pub configfiles: BTreeMap<String, String>,
    pub tool_directory: String,
}

/// A file given to a data input, with its Galaxy datatype and name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataFile {
    pub path: String,
    pub ext: String,
    pub name: String,
}

/// Galaxy's `InputValueWrapper`: text, integer, float, boolean, hidden and color params.
pub struct Input {
    kind: ParamKind,
    raw: Value,
    optional: bool,
    truevalue: String,
    falsevalue: String,
    rgb: bool,
    sanitizer: Option<Sanitizer>,
}

/// Galaxy's `SelectToolParameterWrapper`.
pub struct Select {
    name: String,
    raw: Value,
    multiple: bool,
    separator: String,
    options: Vec<(String, String)>,
    sanitizer: Option<Sanitizer>,
}

/// Galaxy's `DatasetFilenameWrapper`, or its `NoneDataset` when there is no file.
pub struct Dataset {
    file: Option<DataFile>,
    ext: String,
}

/// Galaxy's `DatasetListWrapper` of a data input with `multiple="true"`.
pub struct DatasetList {
    items: Vec<Value>,
}

/// The template values of `tool`'s inputs for `job`, by top-level name.
pub fn wrapped(tool: &Tool, job: &Job) -> Result<Vec<(String, Value)>, Error> {
    walk(tool, job, &tool.inputs, &job.parameters, "", false)
}

/// The raw values that output filters see: Python dicts, numbers, text, booleans, lists, and
/// datasets, by top-level name.
pub fn raw(tool: &Tool, job: &Job) -> Result<Vec<(String, Value)>, Error> {
    walk(tool, job, &tool.inputs, &job.parameters, "", true)
}

fn walk(
    tool: &Tool,
    job: &Job,
    inputs: &[Declared],
    given: &Json,
    prefix: &str,
    raw: bool,
) -> Result<Vec<(String, Value)>, Error> {
    let mut values = Vec::new();
    for input in inputs {
        match input {
            Declared::Param(param) => {
                let path = format!("{prefix}{}", param.name);
                values.push((
                    param.name.clone(),
                    leaf(tool, job, param, given.get(&param.name), &path, raw)?,
                ));
            }
            Declared::Section(section) => {
                let inner = format!("{prefix}{}|", section.name);
                let entries = walk(
                    tool,
                    job,
                    &section.inputs,
                    &given[&section.name],
                    &inner,
                    raw,
                )?;
                values.push((section.name.clone(), Value::dict_of(entries)));
            }
            Declared::Repeat(repeat) => {
                let mut items = Vec::new();
                let given_items = given[&repeat.name].as_array().cloned().unwrap_or_default();
                for (index, item) in given_items.iter().enumerate() {
                    let inner = format!("{prefix}{}_{index}|", repeat.name);
                    items.push(Value::dict_of(walk(
                        tool,
                        job,
                        &repeat.inputs,
                        item,
                        &inner,
                        raw,
                    )?));
                }
                values.push((repeat.name.clone(), Value::list(items)));
            }
            Declared::Conditional(conditional) => {
                let inner = format!("{prefix}{}|", conditional.name);
                let given = &given[&conditional.name];
                let test = &conditional.test;
                let chosen = match given.get(&test.name) {
                    None => test.initial(tool.profile.as_deref()),
                    Some(value) => value.clone(),
                };
                let case_name = galaxy_parser::definition::case_value(test, &chosen);
                let Some(index) = (conditional.cases.iter())
                    .position(|case| Some(case.value.as_str()) == case_name.as_deref())
                else {
                    return Err(Error::Input(format!(
                        "the conditional {} has no case for {chosen}",
                        conditional.name
                    )));
                };
                let path = format!("{inner}{}", test.name);
                let mut entries = vec![(
                    test.name.clone(),
                    leaf(tool, job, test, Some(&chosen), &path, raw)?,
                )];
                entries.extend(walk(
                    tool,
                    job,
                    &conditional.cases[index].inputs,
                    given,
                    &inner,
                    raw,
                )?);
                entries.push(("__current_case__".to_owned(), Value::Int(index as i64)));
                values.push((conditional.name.clone(), Value::dict_of(entries)));
            }
        }
    }
    Ok(values)
}

fn leaf(
    tool: &Tool,
    job: &Job,
    param: &Param,
    given: Option<&Json>,
    path: &str,
    raw: bool,
) -> Result<Value, Error> {
    let profile = tool.profile.as_deref();
    // A missing value takes the default; an explicit null clears it.
    let value = match given {
        None => python(&param.initial(profile)),
        Some(given) => typed(&param.kind, python(given)),
    };
    let sanitizer = || Sanitizer::of(param, tool.sanitize);
    Ok(match &param.kind {
        ParamKind::Data => {
            let files = job.datasets.get(path);
            let one = |file: Option<&DataFile>| {
                Value::native(Dataset {
                    file: file.cloned(),
                    ext: file.map_or_else(|| "data".to_owned(), |file| file.ext.clone()),
                })
            };
            match (files, param.multiple, raw) {
                (None, _, true) => Value::None,
                (Some(files), false, true) => {
                    files.first().map_or(Value::None, |file| one(Some(file)))
                }
                (Some(files), true, true) => {
                    Value::list(files.iter().map(|file| one(Some(file))).collect())
                }
                (None, true, false) => Value::native(DatasetList {
                    items: vec![one(None)],
                }),
                (Some(files), true, false) => Value::native(DatasetList {
                    items: files.iter().map(|file| one(Some(file))).collect(),
                }),
                (files, false, false) => one(files.and_then(|files| files.first())),
            }
        }
        ParamKind::Collection | ParamKind::Other(_) => {
            return Err(Error::Input(format!(
                "the input {path} of type {:?} is not supported yet",
                param.kind
            )));
        }
        _ if raw => value,
        ParamKind::Select => Value::native(Select {
            name: param.name.clone(),
            raw: value,
            multiple: param.multiple,
            separator: param.separator.clone(),
            options: (param.options.iter())
                .map(|option| (option.value.clone(), option.label.clone()))
                .collect(),
            sanitizer: sanitizer(),
        }),
        kind => {
            // Below profile 23.0 an absent text param whose optionality is inferred is "".
            let legacy = param.optional_inferred && !at_least(profile, "23.0");
            let raw_value = match value {
                Value::None if *kind == ParamKind::Text && legacy => Value::str(""),
                other => other,
            };
            Value::native(Input {
                kind: kind.clone(),
                raw: raw_value,
                optional: param.optional,
                truevalue: param.truevalue.clone(),
                falsevalue: param.falsevalue.clone(),
                rgb: param.rgb,
                sanitizer: sanitizer(),
            })
        }
    })
}

/// A JSON value as a Python value.
pub fn python(value: &Json) -> Value {
    match value {
        Json::Null => Value::None,
        Json::Bool(value) => Value::Bool(*value),
        Json::Number(number) => match number.as_i64() {
            Some(integer) => Value::Int(integer),
            None => Value::Float(number.as_f64().unwrap_or(f64::NAN)),
        },
        Json::String(text) => Value::str(text.as_str()),
        Json::Array(items) => Value::list(items.iter().map(python).collect()),
        Json::Object(entries) => Value::dict_of(
            entries
                .iter()
                .map(|(key, value)| (key.clone(), python(value))),
        ),
    }
}

/// A given number as its param's type, like Galaxy's `int()` and `float()` in `to_python`.
fn typed(kind: &ParamKind, value: Value) -> Value {
    match (kind, value) {
        (ParamKind::Float, Value::Int(number)) => Value::Float(number as f64),
        (ParamKind::Integer, Value::Float(number)) if number.is_finite() => {
            Value::Int(number as i64)
        }
        (_, value) => value,
    }
}

/// An output for the template: its path, or an empty dataset when it is not created.
pub fn output(path: Option<&str>, format: Option<&str>) -> Value {
    Value::native(Dataset {
        file: path.map(|path| DataFile {
            path: path.to_owned(),
            ext: format.unwrap_or("data").to_owned(),
            name: String::new(),
        }),
        ext: format.unwrap_or("data").to_owned(),
    })
}

/// Python's truth value of a plain value.
fn truth(value: &Value) -> bool {
    match value.plain() {
        Value::None => false,
        Value::Bool(value) => *value,
        Value::Int(value) => *value != 0,
        Value::Float(value) => *value != 0.0,
        Value::Str(text) => !text.is_empty(),
        Value::List(items) => !items.borrow().is_empty(),
        _ => true,
    }
}

fn swapped(operator: Operator, reflected: bool) -> Operator {
    match (operator, reflected) {
        (Operator::Lt, true) => Operator::Gt,
        (Operator::Gt, true) => Operator::Lt,
        (Operator::LtE, true) => Operator::GtE,
        (Operator::GtE, true) => Operator::LtE,
        (operator, _) => operator,
    }
}

fn parse_float(text: &str) -> Result<f64, Python> {
    text.trim()
        .parse::<f64>()
        .map_err(|_| Python::value_error(format!("could not convert string to float: '{text}'")))
}

impl Input {
    /// `to_param_dict_string`: what the template prints.
    fn printed(&self, interpreter: &mut Interpreter) -> Result<String, Python> {
        let text = match (&self.kind, self.raw.plain()) {
            (ParamKind::Boolean, _) => {
                let checked = match self.raw.plain() {
                    Value::Bool(value) => *value,
                    Value::None => false,
                    other => truthy(&interpreter.to_str(other)?),
                };
                return Ok(if checked {
                    self.truevalue.clone()
                } else {
                    self.falsevalue.clone()
                });
            }
            (ParamKind::Color, Value::Str(color)) if self.rgb => {
                let digits = color.trim_start_matches('#');
                let channel = |at: usize| {
                    digits
                        .get(at..at + 2)
                        .and_then(|hex| u8::from_str_radix(hex, 16).ok())
                };
                return match (channel(0), channel(2), channel(4)) {
                    (Some(red), Some(green), Some(blue)) => Ok(format!("({red}, {green}, {blue})")),
                    _ => Err(Python::value_error(format!(
                        "Failed to convert '{color}' to RGB."
                    ))),
                };
            }
            (ParamKind::Color, value) => return interpreter.to_str(value),
            (_, Value::None) => String::new(),
            (_, value) => interpreter.to_str(value)?,
        };
        Ok(match &self.sanitizer {
            Some(sanitizer) => sanitizer.apply(&text),
            None => text,
        })
    }

    /// `_get_cast_values`: the wrapper cast by its type, beside the other side unchanged.
    fn cast(&self, interpreter: &mut Interpreter, other: &Value) -> Result<Value, Python> {
        if let (ParamKind::Boolean, Some(text)) = (&self.kind, other.as_str()) {
            if text == self.truevalue || text == self.falsevalue {
                return Ok(Value::str(self.printed(interpreter)?));
            }
            return Ok(Value::Bool(truth(&self.raw)));
        }
        if self.optional && self.raw.is_none() {
            return Ok(match other.as_str() {
                Some(_) => Value::str(self.printed(interpreter)?),
                None => Value::None,
            });
        }
        Ok(match self.kind {
            ParamKind::Integer => Value::Int(parse_float(&self.printed(interpreter)?)? as i64),
            ParamKind::Float => Value::Float(parse_float(&self.printed(interpreter)?)?),
            ParamKind::Boolean => Value::Bool(truth(&self.raw)),
            _ => Value::str(self.printed(interpreter)?),
        })
    }

    /// The other side of a comparison with a boolean against text that is not its true or
    /// false value: Galaxy's `string_as_bool`.
    fn other_side(&self, other: &Value) -> Value {
        match (&self.kind, other.as_str()) {
            (ParamKind::Boolean, Some(text))
                if text != self.truevalue && text != self.falsevalue =>
            {
                Value::Bool(truthy(text))
            }
            _ => other.clone(),
        }
    }
}

impl Native for Input {
    fn type_name(&self) -> &str {
        "InputValueWrapper"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get_attribute(
        &self,
        interpreter: &mut Interpreter,
        _: &Value,
        name: &str,
    ) -> Result<Option<Value>, Python> {
        match name {
            "value" => Ok(Some(self.raw.clone())),
            _ => interpreter.get_attribute(&self.raw, name).map(Some),
        }
    }

    fn truthy(&self) -> Option<bool> {
        Some(truth(&self.raw))
    }

    fn to_str(&self, interpreter: &mut Interpreter) -> Result<Option<String>, Python> {
        self.printed(interpreter).map(Some)
    }

    fn iterate(&self, interpreter: &mut Interpreter) -> Result<Option<Vec<Value>>, Python> {
        Ok(Some(vec![Value::str(self.printed(interpreter)?)]))
    }

    fn contains(
        &self,
        interpreter: &mut Interpreter,
        item: &Value,
    ) -> Result<Option<bool>, Python> {
        let printed = Value::str(self.printed(interpreter)?);
        interpreter.equals(item, &printed).map(Some)
    }

    fn binary(
        &self,
        interpreter: &mut Interpreter,
        operator: Operator,
        other: &Value,
        reflected: bool,
    ) -> Result<Option<Value>, Python> {
        let operator = swapped(operator, reflected);
        if !matches!(
            operator,
            Operator::Eq
                | Operator::NotEq
                | Operator::Lt
                | Operator::LtE
                | Operator::Gt
                | Operator::GtE
        ) {
            return Ok(None);
        }
        let this = self.cast(interpreter, other)?;
        let that = self.other_side(other);
        let equal = interpreter.equals(&this, &that)?;
        let mut greater = || -> Result<bool, Python> {
            let result = interpreter.binary(Operator::Gt, &this, &that)?;
            interpreter.truthy(&result)
        };
        let result = match operator {
            Operator::Eq => equal,
            Operator::NotEq => !equal,
            Operator::Gt => greater()?,
            Operator::Lt => !greater()? && !equal,
            Operator::GtE => greater()? || equal,
            _ => !greater()?,
        };
        Ok(Some(Value::Bool(result)))
    }

    fn unary(
        &self,
        interpreter: &mut Interpreter,
        operator: &str,
    ) -> Result<Option<Value>, Python> {
        let mut number =
            || -> Result<f64, Python> { parse_float(&self.printed(&mut *interpreter)?) };
        match operator {
            "int" => Ok(Some(Value::Int(number()? as i64))),
            "float" => Ok(Some(Value::Float(number()?))),
            _ => Ok(None),
        }
    }
}

impl Select {
    fn printed(&self, interpreter: &mut Interpreter) -> Result<String, Python> {
        let clean = |text: String| match &self.sanitizer {
            Some(sanitizer) => sanitizer.apply(&text),
            None => text,
        };
        match self.raw.plain() {
            Value::None => Ok("None".to_owned()),
            Value::List(items) if items.borrow().is_empty() => Ok("None".to_owned()),
            Value::List(_) if !self.multiple => Err(Python::new(
                "Exception",
                "multiple values provided but parameter is not expecting multiple values",
            )),
            Value::List(items) => {
                let items = items.borrow().clone();
                let mut texts = Vec::new();
                for item in &items {
                    texts.push(clean(interpreter.to_str(item)?));
                }
                Ok(texts.join(&self.separator))
            }
            other => Ok(clean(interpreter.to_str(other)?)),
        }
    }

    fn selected(&self) -> Vec<String> {
        match self.raw.plain() {
            Value::List(items) => items
                .borrow()
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect(),
            Value::Str(text) => vec![text.to_string()],
            _ => Vec::new(),
        }
    }
}

impl Native for Select {
    fn type_name(&self) -> &str {
        "SelectToolParameterWrapper"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        _: &Value,
        name: &str,
    ) -> Result<Option<Value>, Python> {
        let selected = self.selected();
        Ok(Some(match name {
            "value" => self.raw.clone(),
            "name" => Value::str(self.name.as_str()),
            "multiple" => Value::Bool(self.multiple),
            "separator" => Value::str(self.separator.as_str()),
            "value_label" => {
                let labels: Vec<&str> = (self.options.iter())
                    .filter(|(value, _)| selected.contains(value))
                    .map(|(_, label)| label.as_str())
                    .collect();
                match labels.is_empty() {
                    true => Value::str("Nothing selected."),
                    false => Value::str(labels.join("\n")),
                }
            }
            "fields" => {
                return Err(Python::unsupported(
                    "the fields of a select, which need data tables",
                ));
            }
            _ => return Ok(None),
        }))
    }

    fn truthy(&self) -> Option<bool> {
        Some(truth(&self.raw))
    }

    fn to_str(&self, interpreter: &mut Interpreter) -> Result<Option<String>, Python> {
        self.printed(interpreter).map(Some)
    }

    fn iterate(&self, _: &mut Interpreter) -> Result<Option<Vec<Value>>, Python> {
        if !self.multiple {
            return Err(Python::new(
                "Exception",
                "Tried to iterate over a non-multiple parameter.",
            ));
        }
        match self.raw.plain() {
            Value::List(items) => Ok(Some(items.borrow().clone())),
            _ => Err(Python::type_error("'NoneType' object is not iterable")),
        }
    }

    fn contains(
        &self,
        interpreter: &mut Interpreter,
        item: &Value,
    ) -> Result<Option<bool>, Python> {
        let items = self.iterate(interpreter)?.unwrap_or_default();
        for known in &items {
            if interpreter.equals(item, known)? {
                return Ok(Some(true));
            }
        }
        Ok(Some(false))
    }

    fn binary(
        &self,
        interpreter: &mut Interpreter,
        operator: Operator,
        other: &Value,
        reflected: bool,
    ) -> Result<Option<Value>, Python> {
        let equal = |interpreter: &mut Interpreter| -> Result<bool, Python> {
            let Some(text) = other.as_str() else {
                return Ok(false);
            };
            let empty = match self.raw.plain() {
                Value::None => true,
                Value::List(items) => items.borrow().is_empty(),
                _ => false,
            };
            Ok((text.is_empty() && empty) || self.printed(interpreter)? == text)
        };
        Ok(match (operator, reflected) {
            (Operator::Eq, _) => Some(Value::Bool(equal(interpreter)?)),
            (Operator::NotEq, _) => Some(Value::Bool(!equal(interpreter)?)),
            (Operator::Add, false) => {
                let tail = interpreter.to_str(other)?;
                Some(Value::str(format!("{}{tail}", self.printed(interpreter)?)))
            }
            _ => None,
        })
    }
}

impl Native for Dataset {
    fn type_name(&self) -> &str {
        match self.file {
            Some(_) => "DatasetFilenameWrapper",
            None => "NoneDataset",
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        _: &Value,
        name: &str,
    ) -> Result<Option<Value>, Python> {
        let Some(file) = &self.file else {
            return Ok(Some(match name {
                "ext" | "extension" => Value::str(self.ext.as_str()),
                "dbkey" => Value::str("?"),
                _ => Value::str("None"),
            }));
        };
        Ok(Some(match name {
            "ext" | "extension" | "file_ext" => Value::str(file.ext.as_str()),
            "name" | "element_identifier" => Value::str(file.name.as_str()),
            "file_name" => Value::str(file.path.as_str()),
            _ => return Err(Python::unsupported(format!("the dataset attribute {name}"))),
        }))
    }

    fn truthy(&self) -> Option<bool> {
        Some(self.file.is_some())
    }

    fn to_str(&self, _: &mut Interpreter) -> Result<Option<String>, Python> {
        Ok(Some(self.file.as_ref().map_or_else(
            || "None".to_owned(),
            |file| file.path.clone(),
        )))
    }
}

impl Native for DatasetList {
    fn type_name(&self) -> &str {
        "DatasetListWrapper"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn truthy(&self) -> Option<bool> {
        Some(self.items.iter().any(truth_of))
    }

    fn length(&self) -> Option<usize> {
        Some(self.items.len())
    }

    fn iterate(&self, _: &mut Interpreter) -> Result<Option<Vec<Value>>, Python> {
        Ok(Some(self.items.clone()))
    }

    fn get_item(
        &self,
        _: &mut Interpreter,
        _: &Value,
        key: &Value,
    ) -> Result<Option<Value>, Python> {
        let length = self.items.len() as i64;
        let Some(index) = key.as_int() else {
            return Err(Python::type_error("list indices must be integers"));
        };
        let at = if index < 0 { index + length } else { index };
        match usize::try_from(at).ok().and_then(|at| self.items.get(at)) {
            Some(item) => Ok(Some(item.clone())),
            None => Err(Python::index_error("list index out of range")),
        }
    }

    fn to_str(&self, interpreter: &mut Interpreter) -> Result<Option<String>, Python> {
        let mut texts = Vec::new();
        for item in &self.items {
            texts.push(interpreter.to_str(item)?);
        }
        Ok(Some(texts.join(",")))
    }
}

fn truth_of(value: &Value) -> bool {
    value
        .as_native::<Dataset>()
        .is_some_and(|dataset| dataset.file.is_some())
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use python_evaluator::LocalHost;
    use serde_json::json;

    use super::*;

    /// The text of each Python expression over the values of params declared in `inputs`.
    fn shown(
        inputs: &str,
        profile: &str,
        parameters: Json,
        files: &[(&str, &[&str])],
        expressions: &[&str],
    ) -> Vec<String> {
        let xml = format!("<tool id=\"t\" profile=\"{profile}\"><inputs>{inputs}</inputs></tool>");
        let tool = galaxy_parser::tool::parse(&xml, "t.xml", &mut |_| unreachable!("no imports"))
            .expect("a tool");
        let mut job = Job {
            parameters,
            ..Job::default()
        };
        for (path, names) in files {
            let files = (names.iter())
                .map(|name| DataFile {
                    path: (*name).to_owned(),
                    ext: "txt".to_owned(),
                    name: (*name).to_owned(),
                })
                .collect();
            job.datasets.insert((*path).to_owned(), files);
        }
        let mut interpreter = Interpreter::new(Rc::new(LocalHost::new("/")));
        let globals = interpreter.new_globals("values", None);
        for (name, value) in wrapped(&tool, &job).expect("values") {
            globals.set(&name, value);
        }
        (expressions.iter())
            .map(|expression| {
                let value = interpreter
                    .eval_text(expression, &globals)
                    .expect(expression);
                interpreter.to_str(&value).expect("text")
            })
            .collect()
    }

    #[test]
    fn text_rows() {
        let param = r#"<param name="v" type="text" optional="true"/>"#;
        let row = |value: Json| {
            shown(
                param,
                "25.1",
                value,
                &[],
                &["str(v)", "bool(v)", "v == ''", "v.value is None"],
            )
        };
        assert_eq!(
            row(json!({"v": "it's"})),
            ["it__sq__s", "True", "False", "False"]
        );
        assert_eq!(row(json!({"v": ""})), ["", "False", "True", "False"]);
        assert_eq!(row(json!({})), ["", "False", "True", "True"]);
        let inferred = r#"<param name="v" type="text"/>"#;
        let legacy = shown(
            inferred,
            "22.05",
            json!({}),
            &[],
            &["str(v)", "v.value is None"],
        );
        assert_eq!(
            legacy,
            ["", "False"],
            "below 23.0 an inferred optional text is \"\""
        );
    }

    #[test]
    fn number_rows() {
        let param = r#"<param name="v" type="integer" optional="true"/>"#;
        let compared = [
            "str(v)", "bool(v)", "v == ''", "v == 5", "v == '5'", "v > 3", "3 < v",
        ];
        let five = shown(param, "25.1", json!({"v": 5}), &[], &compared);
        assert_eq!(
            five,
            ["5", "True", "False", "True", "False", "True", "True"]
        );
        let zero = shown(
            param,
            "25.1",
            json!({"v": 0}),
            &[],
            &["str(v)", "bool(v)", "v == ''"],
        );
        assert_eq!(zero, ["0", "False", "False"]);
        let absent = shown(
            param,
            "25.1",
            json!({}),
            &[],
            &["str(v)", "bool(v)", "v == ''"],
        );
        assert_eq!(absent, ["", "False", "True"]);
        let float = r#"<param name="v" type="float"/>"#;
        let quarter = shown(
            float,
            "25.1",
            json!({"v": 0.25}),
            &[],
            &["str(v)", "float(v) * 2"],
        );
        assert_eq!(quarter, ["0.25", "0.5"]);
        let nothing = shown(
            float,
            "25.1",
            json!({"v": 0.0}),
            &[],
            &["str(v)", "bool(v)"],
        );
        assert_eq!(nothing, ["0.0", "False"]);
    }

    #[test]
    fn boolean_rows() {
        let param = r#"<param name="v" type="boolean" truevalue="--yes" falsevalue="--no" optional="true"/>"#;
        let compared = ["str(v)", "bool(v)", "v == '--yes'", "v == 'true'"];
        let row = |value: Json| shown(param, "26.2", value, &[], &compared);
        assert_eq!(row(json!({"v": true})), ["--yes", "True", "True", "True"]);
        assert_eq!(
            row(json!({"v": false})),
            ["--no", "False", "False", "False"]
        );
        assert_eq!(
            row(json!({})),
            ["--no", "False", "False", "False"],
            "absent is unchecked"
        );
    }

    #[test]
    fn select_rows() {
        let params = r#"<param name="v" type="select" optional="true"><option value="a b">A B</option><option value="c'd">C D</option></param>
            <param name="m" type="select" multiple="true" separator=" "><option value="x"/><option value="y"/></param>"#;
        let compared = [
            "str(v)",
            "bool(v)",
            "v == ''",
            "str(m)",
            "bool(m)",
            "m == ''",
            "v.value_label",
        ];
        let row = |value: Json| shown(params, "25.1", value, &[], &compared);
        let given = row(json!({"v": "c'd", "m": ["x", "y"]}));
        assert_eq!(
            given,
            ["c__sq__d", "True", "False", "x y", "True", "False", "C D"]
        );
        let empty = row(json!({"m": []}));
        assert_eq!(
            empty,
            [
                "None",
                "False",
                "True",
                "None",
                "False",
                "True",
                "Nothing selected."
            ]
        );
    }

    #[test]
    fn numbers_follow_kind() {
        let params = r#"<param name="f" type="float"/><param name="i" type="integer"/>"#;
        let given = json!({"f": 1, "i": 2.0});
        let printed = shown(params, "25.1", given.clone(), &[], &["str(f)", "str(i)"]);
        assert_eq!(
            printed,
            ["1.0", "2"],
            "Galaxy casts a float param with float()"
        );
        let xml = format!("<tool id=\"t\"><inputs>{params}</inputs></tool>");
        let tool = galaxy_parser::tool::parse(&xml, "t.xml", &mut |_| unreachable!("no imports"))
            .expect("a tool");
        let job = Job {
            parameters: given,
            ..Job::default()
        };
        let raw = raw(&tool, &job).expect("raw values");
        assert!(
            matches!(raw[0].1, Value::Float(number) if number == 1.0),
            "filters see a float"
        );
        assert!(matches!(raw[1].1, Value::Int(2)), "filters see an int");
    }

    #[test]
    fn null_clears_default() {
        let param = r#"<param name="s" type="select" optional="true">
            <option value="a"/><option value="b" selected="true"/></param>"#;
        let row = |value: Json| shown(param, "25.1", value, &[], &["str(s)", "bool(s)"]);
        assert_eq!(
            row(json!({})),
            ["b", "True"],
            "a missing value takes the default"
        );
        assert_eq!(row(json!({"s": null})), ["None", "False"], "null clears it");
    }

    #[test]
    fn dataset_rows() {
        let params = r#"<param name="v" type="data" optional="true"/>
            <param name="l" type="data" multiple="true" optional="true"/>"#;
        let compared = ["str(v)", "bool(v)", "str(l)", "bool(l)", "len(l)"];
        let row = |files: &[(&str, &[&str])]| shown(params, "25.1", json!({}), files, &compared);
        let given = row(&[("v", &["in/a.txt"]), ("l", &["p1", "p2"])]);
        assert_eq!(given, ["in/a.txt", "True", "p1,p2", "True", "2"]);
        assert_eq!(
            row(&[("l", &[])]),
            ["None", "False", "", "False", "0"],
            "empty list"
        );
        assert_eq!(
            row(&[]),
            ["None", "False", "None", "False", "1"],
            "absent becomes [None]"
        );
    }
}
