//! Reads and checks nf-cmgg worksheets, which describe the sample sheets of a pipeline.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_evaluator::interpreter::parse_yaml;
use nextflow_evaluator::regex::Pattern;
use nextflow_evaluator::value::Value;

use super::expression::{Code, references};
use crate::nf_schema::entries;

/// The type of an input or sample sheet field.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Type {
    String,
    Integer,
    Float,
    Boolean,
}

impl Type {
    fn parse(text: &str) -> Option<Type> {
        match text {
            "string" => Some(Type::String),
            "integer" => Some(Type::Integer),
            "float" => Some(Type::Float),
            "boolean" => Some(Type::Boolean),
            _ => None,
        }
    }

    pub(super) fn name(self) -> &'static str {
        match self {
            Type::String => "string",
            Type::Integer => "integer",
            Type::Float => "float",
            Type::Boolean => "boolean",
        }
    }
}

const TYPES: &str = "string, integer, float, boolean";

#[derive(Debug)]
pub(super) struct Input {
    pub name: String,
    /// The column of the input sample sheet.
    pub source: String,
    pub default: Value,
    pub kind: Type,
}

/// A field of the `values` section: a constant or an expression.
#[derive(Debug)]
pub(super) enum Computed {
    Constant(Value),
    Function(Code),
}

#[derive(Debug)]
pub(super) struct Output {
    pub name: String,
    pub pattern: Pattern,
}

#[derive(Debug)]
pub(super) struct Metric {
    pub name: String,
    pub pattern: Pattern,
    pub subpath: Option<String>,
    pub filetype: String,
    /// The column with the sample ID, and the column with the value.
    pub id: String,
    pub field: String,
}

#[derive(Debug)]
pub(super) struct Field {
    pub key: String,
    pub source: String,
    pub kind: Option<Type>,
}

#[derive(Debug)]
pub(super) struct Sheet {
    pub name: String,
    pub include: Option<Code>,
    pub filter: Option<Code>,
    pub fields: Vec<Field>,
}

#[derive(Debug)]
pub(super) struct Worksheet {
    pub id_field: String,
    pub inputs: Vec<Input>,
    pub values: Vec<(String, Computed)>,
    pub outputs: Vec<Output>,
    pub metrics: Vec<Metric>,
    pub sheets: Vec<Sheet>,
    pub split_by: Option<String>,
}

/// The top-level `name` of a worksheet file, if it reads.
pub(super) fn pipeline(text: &str) -> Option<String> {
    let document = parse_yaml(text).ok()?;
    let name = map_get(&document, "name")?;
    Some(name.to_string())
}

/// Reads and checks a worksheet; the error is the collected message of all its problems.
pub(super) fn read(file: &str, text: &str) -> Result<Worksheet, String> {
    let document = parse_yaml(text)
        .map_err(|error| format!("Failed to parse worksheet {file}: {}", error.message))?;
    if matches!(document, Value::Null) {
        return Err(format!("Worksheet {file} is empty"));
    }
    for key in ["name", "id_field", "input", "samplesheets"] {
        if map_get(&document, key).is_none() {
            return Err(format!(
                "Worksheet {file} is missing the required '{key}' field"
            ));
        }
    }
    let mut check = Check {
        fields: Vec::new(),
        sections: Vec::new(),
    };
    let inputs = check.inputs(&document);
    let id_field = text_of(map_get(&document, "id_field"));
    if inputs.is_some() && !(inputs.iter().flatten()).any(|input| input.source == id_field) {
        let message = format!(
            "Worksheet {file} 'id_field' '{id_field}' is not defined as a source or key (when source is missing) in the input block"
        );
        check.fail("input", vec![message]);
    }
    let outputs = check.patterns(&document, "output");
    let metrics = check.metrics(&document);
    let values = check.values(&document);
    let sheets = check.sheets(&document);
    let split_by = map_get(&document, "samplesheets_settings")
        .and_then(|settings| map_get(&settings, "split_by"))
        .map(|field| field.to_string())
        .filter(|field| {
            let known = check.fields.contains(field);
            if !known {
                let fields = check.fields.join(", ");
                tracing::error!("{field} is not a valid `split_by` field, use one of: {fields}");
            }
            known
        });
    if !check.sections.is_empty() {
        let name = std::path::Path::new(file)
            .file_name()
            .map_or(file.into(), |n| n.to_string_lossy());
        let mut message = format!("Worksheet {name} is invalid:");
        for (section, errors) in &check.sections {
            message.push_str(&format!("\n- Invalid worksheet {section}:"));
            for error in errors {
                message.push_str(&format!("\n- {error}"));
            }
        }
        return Err(message);
    }
    Ok(Worksheet {
        id_field,
        inputs: inputs.unwrap_or_default(),
        values: values.unwrap_or_default(),
        outputs: outputs.unwrap_or_default(),
        metrics: metrics.unwrap_or_default(),
        sheets: sheets.unwrap_or_default(),
        split_by,
    })
}

/// The data fields known so far and the errors of each section.
struct Check {
    fields: Vec<String>,
    sections: Vec<(&'static str, Vec<String>)>,
}

impl Check {
    fn fail(&mut self, section: &'static str, errors: Vec<String>) {
        for error in &errors {
            tracing::error!("{error}");
        }
        match self.sections.iter_mut().find(|(name, _)| *name == section) {
            Some((_, known)) => known.extend(errors),
            None => self.sections.push((section, errors)),
        }
    }

    /// The entries of a map section; `None` with an error when it is given but empty.
    fn section(&mut self, document: &Value, name: &'static str) -> Option<Vec<(String, Value)>> {
        let section = map_get(document, name)?;
        let fields: Vec<(String, Value)> = (entries(&section).into_iter())
            .filter(|(key, _)| key != "description")
            .collect();
        if fields.is_empty() {
            self.fail(name, vec![format!("Worksheet {name} is missing or empty")]);
            return None;
        }
        Some(fields)
    }

    /// Adds the keys of a section that checked, as data fields of the later sections.
    fn passed<T>(
        &mut self,
        errors: Vec<String>,
        section: &'static str,
        keys: Vec<String>,
        items: T,
    ) -> Option<T> {
        if errors.is_empty() {
            self.fields.extend(keys);
            Some(items)
        } else {
            self.fail(section, errors);
            None
        }
    }

    fn inputs(&mut self, document: &Value) -> Option<Vec<Input>> {
        let section = self.section(document, "input")?;
        let (mut errors, mut inputs) = (Vec::new(), Vec::new());
        for (name, entry) in &section {
            let kind = text_or(map_get(entry, "type"), "string");
            let Some(kind) = Type::parse(&kind) else {
                errors.push(format!(
                    "Invalid type '{kind}' for input field '{name}'. Expected one of: {TYPES}"
                ));
                continue;
            };
            inputs.push(Input {
                name: name.clone(),
                source: text_or(map_get(entry, "source"), name),
                default: map_get(entry, "default").unwrap_or(Value::Null),
                kind,
            });
        }
        let keys = section.into_iter().map(|(name, _)| name).collect();
        self.passed(errors, "input", keys, inputs)
    }

    fn patterns(&mut self, document: &Value, section_name: &'static str) -> Option<Vec<Output>> {
        let section = self.section(document, section_name)?;
        let (mut errors, mut outputs) = (Vec::new(), Vec::new());
        for (name, entry) in &section {
            let Some(text) = map_get(entry, "pattern").map(|pattern| pattern.to_string()) else {
                errors.push(format!(
                    "Output field '{name}' is missing the required 'pattern' field"
                ));
                continue;
            };
            match Pattern::new(&text) {
                Ok(pattern) => outputs.push(Output {
                    name: name.clone(),
                    pattern,
                }),
                Err(error) => errors.push(format!(
                    "Invalid regex pattern '{text}' for output field '{name}': {}",
                    error.message
                )),
            }
        }
        let keys = section.into_iter().map(|(name, _)| name).collect();
        self.passed(errors, section_name, keys, outputs)
    }

    fn metrics(&mut self, document: &Value) -> Option<Vec<Metric>> {
        let section = self.section(document, "metrics")?;
        let (mut errors, mut metrics) = (Vec::new(), Vec::new());
        for (name, entry) in &section {
            let filetype = text_or(map_get(entry, "filetype"), "tsv");
            let mut fine = true;
            if !["tsv", "csv", "yml", "yaml", "json"].contains(&filetype.as_str()) {
                errors.push(format!(
                    "Invalid filetype '{filetype}' for metric field '{name}'. Expected one of: tsv, csv, yml, yaml, json"
                ));
                fine = false;
            }
            let pattern = match map_get(entry, "pattern").map(|pattern| pattern.to_string()) {
                None => None,
                Some(text) => match Pattern::new(&text) {
                    Ok(pattern) => Some(pattern),
                    Err(error) => {
                        errors.push(format!(
                            "Invalid regex pattern '{text}' for output field '{name}': {}",
                            error.message
                        ));
                        fine = false;
                        None
                    }
                },
            };
            let mut required = |key: &str, present: bool| {
                if !present {
                    errors.push(format!(
                        "Metric field '{name}' is missing the required '{key}' field"
                    ));
                    fine = false;
                }
            };
            required("pattern", map_get(entry, "pattern").is_some());
            required("id", map_get(entry, "id").is_some());
            required("field", map_get(entry, "field").is_some());
            if let (true, Some(pattern)) = (fine, pattern) {
                metrics.push(Metric {
                    name: name.clone(),
                    pattern,
                    subpath: map_get(entry, "subpath").map(|path| path.to_string()),
                    filetype,
                    id: text_of(map_get(entry, "id")),
                    field: text_of(map_get(entry, "field")),
                });
            }
        }
        let keys = section.into_iter().map(|(name, _)| name).collect();
        self.passed(errors, "metrics", keys, metrics)
    }

    fn values(&mut self, document: &Value) -> Option<Vec<(String, Computed)>> {
        let section = self.section(document, "values")?;
        let (mut errors, mut values) = (Vec::new(), Vec::new());
        for (name, entry) in &section {
            let (value, function) = (map_get(entry, "value"), map_get(entry, "func"));
            match (value, function) {
                (Some(_), Some(_)) => errors.push(format!(
                    "Values field '{name}' cannot define both 'value' and 'func'"
                )),
                (None, None) => errors.push(format!(
                    "Values field '{name}' must define either 'value' or 'func'"
                )),
                (Some(value), None) => values.push((name.clone(), Computed::Constant(value))),
                (None, Some(function)) => {
                    let text = function.to_string();
                    let unknown = self.unknown(&text, "input");
                    if let Some(field) = unknown.first() {
                        errors.push(format!(
                            "Values field '{name}' references unknown data field 'data.{field}'. Available data fields: {}",
                            self.available()
                        ));
                        continue;
                    }
                    match Code::compile(&text) {
                        Ok(code) => values.push((name.clone(), Computed::Function(code))),
                        Err(message) => errors.push(format!(
                            "Invalid Groovy expression in values field '{name}': {message}"
                        )),
                    }
                }
            }
        }
        let keys = section.into_iter().map(|(name, _)| name).collect();
        self.passed(errors, "values", keys, values)
    }

    /// The sample sheets that check; one with errors is skipped, and none left is an error.
    fn sheets(&mut self, document: &Value) -> Option<Vec<Sheet>> {
        let list = match map_get(document, "samplesheets") {
            Some(Value::List(list)) => list.borrow().items.clone(),
            _ => Vec::new(),
        };
        let mut sheets = Vec::new();
        for entry in &list {
            let name = text_of(map_get(entry, "name"));
            let mut errors = Vec::new();
            let code = |key: &str, errors: &mut Vec<String>| {
                let text = map_get(entry, key)?.to_string();
                if let Some(field) = self.unknown(&text, "data").first() {
                    errors.push(format!(
                        "Samplesheet '{name}' {key} references unknown data field 'data.{field}'. Available data fields: {}",
                        self.available()
                    ));
                    return None;
                }
                Code::compile(&text)
                    .map_err(|message| {
                        errors.push(format!(
                            "Invalid Groovy expression in samplesheet '{name}' {key}: {message}"
                        ))
                    })
                    .ok()
            };
            let include = code("include_func", &mut errors);
            let filter = code("filter_func", &mut errors);
            let mut fields = Vec::new();
            for (key, field) in map_get(entry, "fields")
                .map(|fields| entries(&fields))
                .unwrap_or_default()
            {
                let source = text_or(map_get(&field, "source"), &key);
                if !self.fields.contains(&source) {
                    errors.push(format!(
                        "Invalid source '{source}' for samplesheet field '{key}'. Available data fields: {}",
                        self.available()
                    ));
                }
                let kind = text_or(map_get(&field, "type"), "");
                let parsed = Type::parse(&kind);
                if !kind.is_empty() && parsed.is_none() {
                    errors.push(format!(
                        "Invalid type '{kind}' for samplesheet field '{key}'. Expected one of: {TYPES}"
                    ));
                }
                fields.push(Field {
                    key,
                    source,
                    kind: parsed,
                });
            }
            if errors.is_empty() {
                sheets.push(Sheet {
                    name,
                    include,
                    filter,
                    fields,
                });
            } else {
                for error in &errors {
                    tracing::error!("{error}");
                }
                tracing::error!("Skipping samplesheet '{name}' due to validation errors");
            }
        }
        if sheets.is_empty() {
            let error = "No valid samplesheet entries remain after validation".to_owned();
            self.fail("samplesheets", vec![error]);
            return None;
        }
        Some(sheets)
    }

    fn unknown(&self, text: &str, prefix: &str) -> Vec<String> {
        let names = references(text, prefix).into_iter();
        names.filter(|name| !self.fields.contains(name)).collect()
    }

    fn available(&self) -> String {
        let mut fields = self.fields.clone();
        fields.sort();
        fields.join(", ")
    }
}

/// The value of a key of a map; null values count as absent.
pub(super) fn map_get(map: &Value, key: &str) -> Option<Value> {
    let Value::Map(map) = map else {
        return None;
    };
    let entries = map.borrow();
    let found = entries
        .entries
        .iter()
        .find(|(name, _)| name.0.to_string() == key);
    found
        .map(|(_, value)| value.clone())
        .filter(|value| !matches!(value, Value::Null))
}

fn text_of(value: Option<Value>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
}

fn text_or(value: Option<Value>, default: &str) -> String {
    value.map_or_else(|| default.to_owned(), |value| value.to_string())
}

