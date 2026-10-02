//! `samplesheetToList`: the entries of a CSV, TSV, JSON or YAML samplesheet, checked against
//! a schema and converted by it, as nf-schema does.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_evaluator::interpreter::{Error, parse_json, parse_yaml};
use nextflow_evaluator::number::{BigDecimal, Number};
use nextflow_evaluator::value::{PathValue, Value};

use super::{entries, errors, get, json, path, plain, read, schema, thrown};
use crate::Context;

const PATH_FORMATS: &[&str] = &["file-path", "path", "directory-path", "file-path-pattern"];

/// Reads the samplesheet, checks it, then converts each entry: a map becomes the list of its
/// fields in schema order, with the `meta` fields in a map first, and text of a path format
/// becomes a path.
pub(super) fn to_list(
    samplesheet: &Value,
    schema_path: &Value,
    _options: Option<&Value>,
    context: &mut Context,
) -> Result<Value, Error> {
    let launch = context.host.launch_dir();
    let file = path(samplesheet, &launch);
    let schema = schema(schema_path, context)?;
    reject_nested_ids(&schema.tree, true)?;
    let mut converter = Converter {
        launch,
        root: schema.tree.clone(),
        unrecognised: Vec::new(),
    };
    let root = converter.resolve(&schema.tree)?;
    let items = get(&root, "items").unwrap_or(Value::Null);
    let text = read(&file, context)?;
    let object = file_object(&file, &text, &root, &converter)?;
    check(&schema.json, &without_nulls(json(&object)), &file, context)?;
    let Value::List(list) = &object else {
        return Err(thrown(format!(
            "The samplesheet {} is not a list",
            file.uri()
        )));
    };
    let mut result = Vec::new();
    for entry in list.borrow().items.iter() {
        let mut meta = Vec::new();
        let converted = converter.format_entry(entry, &items, "", &mut meta)?;
        result.push(match converted {
            _ if meta.is_empty() => converted,
            Value::List(fields) => {
                fields.borrow_mut().items.insert(0, Value::map(meta));
                Value::List(fields)
            }
            other => Value::list(vec![Value::map(meta), other]),
        });
    }
    if !converter.unrecognised.is_empty() {
        let headers: Vec<String> = converter
            .unrecognised
            .iter()
            .map(|header| format!("\t- {header}"))
            .collect();
        context.host.print(&format!(
            "WARN: Found the following unidentified headers in {}:\n{}\n\n",
            file.uri(),
            headers.join("\n")
        ));
    }
    Ok(Value::list(result))
}

struct Converter {
    launch: String,
    root: Value,
    /// The fields that the schema does not know, as nf-schema names them, like `a.b`.
    unrecognised: Vec<String>,
}

impl Converter {
    /// An entry for the result: maps become lists of their fields, and `meta` fields go into
    /// `meta` instead.
    fn format_entry(
        &mut self,
        input: &Value,
        schema: &Value,
        prefix: &str,
        meta: &mut Vec<(Value, Value)>,
    ) -> Result<Value, Error> {
        let schema = self.resolve(schema)?;
        let input = self.with_default(input, &schema)?;
        match &input {
            Value::Map(_) => {
                let properties = self.properties(&input, &schema, prefix)?;
                let mut result = Vec::new();
                for (property, definition) in &properties {
                    let definition = self.resolve(definition)?;
                    self.reject_composed(&definition, "meta", 0)?;
                    let value = get(&input, property).unwrap_or(Value::Null);
                    let prefix = format!("{prefix}{property}.");
                    let ids = match get(&definition, "meta") {
                        Some(Value::List(ids)) => ids.borrow().items.clone(),
                        Some(id @ Value::String(_)) => vec![id],
                        _ => Vec::new(),
                    };
                    if ids.is_empty() {
                        result.push(self.format_entry(&value, &definition, &prefix, meta)?);
                    }
                    for id in ids {
                        let converted = self.process_meta(&value, &definition, &prefix)?;
                        meta.push((Value::string(id.to_string()), converted));
                    }
                }
                Ok(Value::list(result))
            }
            Value::List(list) => {
                self.reject_composed(&schema, "items", 0)?;
                let items = get(&schema, "items").unwrap_or(Value::Null);
                let entries = list.borrow().items.clone();
                let mut converted = Vec::new();
                for (count, entry) in entries.iter().enumerate() {
                    converted.push(self.format_entry(
                        entry,
                        &items,
                        &format!("{prefix}{count}."),
                        meta,
                    )?);
                }
                Ok(Value::list(converted))
            }
            other => self.process_value(other, &schema),
        }
    }

    /// A value for the `meta` map: maps stay maps.
    fn process_meta(
        &mut self,
        input: &Value,
        schema: &Value,
        prefix: &str,
    ) -> Result<Value, Error> {
        let schema = self.resolve(schema)?;
        let input = self.with_default(input, &schema)?;
        match &input {
            Value::Map(_) => {
                let properties = self.properties(&input, &schema, prefix)?;
                let mut converted = Vec::new();
                for (property, definition) in &properties {
                    let value = get(&input, property).unwrap_or(Value::Null);
                    let prefix = format!("{prefix}{property}.");
                    let value = self.process_meta(&value, definition, &prefix)?;
                    converted.push((Value::string(property.as_str()), value));
                }
                Ok(Value::map(converted))
            }
            Value::List(list) => {
                self.reject_composed(&schema, "items", 0)?;
                let items = get(&schema, "items").unwrap_or(Value::Null);
                let entries = list.borrow().items.clone();
                let mut converted = Vec::new();
                for (count, entry) in entries.iter().enumerate() {
                    converted.push(self.process_meta(
                        entry,
                        &items,
                        &format!("{prefix}{count}."),
                    )?);
                }
                Ok(Value::list(converted))
            }
            other => self.process_value(other, &schema),
        }
    }

    /// The properties of the schema, noting the fields of the input that it does not know.
    fn properties(
        &mut self,
        input: &Value,
        schema: &Value,
        prefix: &str,
    ) -> Result<Vec<(String, Value)>, Error> {
        let schema = self.resolve(schema)?;
        self.reject_composed(&schema, "properties", 0)?;
        let properties = get(&schema, "properties").map_or_else(Vec::new, |p| entries(&p));
        for (key, _) in entries(input) {
            let header = format!("{prefix}{key}");
            let known = properties.iter().any(|(name, _)| *name == key);
            if !known && !self.unrecognised.contains(&header) {
                self.unrecognised.push(header);
            }
        }
        Ok(properties)
    }

    /// Text becomes a path when the schema, or one of its `anyOf`, `oneOf` and `allOf`
    /// choices, is a string of a path format. When other string choices have no path
    /// format, only text that looks like a file name becomes a path.
    fn process_value(&self, value: &Value, schema: &Value) -> Result<Value, Error> {
        let Value::String(text) = value else {
            return Ok(value.clone());
        };
        let schema = self.resolve(schema)?;
        let default_format = get(&schema, "format").map(|format| format.to_string());
        let (mut path_format, mut other_format) = (false, false);
        let mut look = |entry: &Value| {
            if get(entry, "type").is_some_and(|kind| kind.to_string() == "string") {
                let format = get(entry, "format").map(|format| format.to_string());
                let format = format
                    .or_else(|| default_format.clone())
                    .unwrap_or_default();
                if PATH_FORMATS.contains(&format.as_str()) {
                    path_format = true;
                } else {
                    other_format = true;
                }
            }
        };
        look(&schema);
        for option in ["anyOf", "oneOf", "allOf"] {
            if let Some(Value::List(choices)) = get(&schema, option) {
                for choice in &choices.borrow().items {
                    look(&self.resolve(choice)?);
                }
            }
        }
        let characters: Vec<char> = text.chars().collect();
        let looks_like_file = text.contains('/')
            || (characters.len() >= 7 && characters[characters.len() - 7..].contains(&'.'))
            || (characters.len() < 7 && text.contains('.'));
        if path_format && (!other_format || looks_like_file) {
            return Ok(Value::Path(std::rc::Rc::new(PathValue::parse(
                text,
                &self.launch,
            ))));
        }
        Ok(value.clone())
    }

    fn with_default(&self, input: &Value, schema: &Value) -> Result<Value, Error> {
        match input {
            Value::Null => {
                let schema = self.resolve(schema)?;
                self.reject_composed(&schema, "default", 0)?;
                Ok(get(&schema, "default").unwrap_or_else(|| Value::list(Vec::new())))
            }
            other => Ok(other.clone()),
        }
    }

    fn resolve(&self, schema: &Value) -> Result<Value, Error> {
        let mut current = schema.clone();
        let mut seen = Vec::new();
        loop {
            let Some(reference) = get(&current, "$ref") else {
                return Ok(current);
            };
            let Value::String(reference) = reference else {
                return Err(thrown(
                    "Invalid samplesheet schema: `$ref` must be text".into(),
                ));
            };
            let reference = reference.to_string();
            if reference.contains('%') {
                return Err(unsupported(format!(
                    "converting the percent-encoded samplesheet schema reference `{reference}`"
                )));
            }
            let allowed = [
                "$ref",
                "$defs",
                "definitions",
                "$schema",
                "$id",
                "$comment",
                "title",
                "description",
                "deprecated",
                "readOnly",
                "writeOnly",
                "examples",
            ];
            let sibling = entries(&current)
                .into_iter()
                .map(|(name, _)| name)
                .find(|name| !allowed.contains(&name.as_str()));
            if let Some(sibling) = sibling {
                return Err(unsupported(format!(
                    "converting a samplesheet schema with both `$ref` and `{sibling}`"
                )));
            }
            if reference != "#" && !reference.starts_with("#/") {
                let kind = if reference.starts_with('#') {
                    "anchor"
                } else {
                    "external"
                };
                return Err(unsupported(format!(
                    "converting a samplesheet schema with an {kind} reference `{reference}`"
                )));
            }
            if seen.contains(&reference) {
                return Err(unsupported(format!(
                    "converting the cyclic samplesheet schema reference `{reference}`"
                )));
            }
            seen.push(reference.clone());
            current = reference_target(&self.root, &reference).map_err(|problem| {
                thrown(format!(
                    "Invalid samplesheet schema reference `{reference}`: {problem}"
                ))
            })?;
        }
    }

    fn reject_composed(&self, schema: &Value, keyword: &str, depth: usize) -> Result<(), Error> {
        if depth > 32 {
            return Err(unsupported(
                "converting recursive samplesheet schema compositions",
            ));
        }
        for composition in ["anyOf", "oneOf", "allOf"] {
            if let Some(Value::List(choices)) = get(schema, composition) {
                for choice in &choices.borrow().items {
                    let choice = self.resolve(choice)?;
                    if get(&choice, keyword).is_some() {
                        return Err(unsupported(format!(
                            "converting samplesheet schema `{keyword}` under `{composition}`"
                        )));
                    }
                    self.reject_composed(&choice, keyword, depth + 1)?;
                }
            }
        }
        Ok(())
    }
}

fn reference_target(root: &Value, reference: &str) -> Result<Value, String> {
    if reference == "#" {
        return Ok(root.clone());
    }
    let mut current = root.clone();
    for token in reference[2..].split('/') {
        let token = pointer_token(token)?;
        current = match &current {
            Value::Map(_) => get(&current, &token),
            Value::List(list) => token
                .parse::<usize>()
                .ok()
                .and_then(|index| list.borrow().items.get(index).cloned()),
            _ => None,
        }
        .ok_or_else(|| format!("pointer segment `{token}` does not exist"))?;
    }
    Ok(current)
}

fn pointer_token(token: &str) -> Result<String, String> {
    let mut decoded = String::new();
    let mut characters = token.chars();
    while let Some(character) = characters.next() {
        if character != '~' {
            decoded.push(character);
            continue;
        }
        match characters.next() {
            Some('0') => decoded.push('~'),
            Some('1') => decoded.push('/'),
            _ => return Err(format!("invalid JSON Pointer escape in `{token}`")),
        }
    }
    Ok(decoded)
}

fn reject_nested_ids(value: &Value, root: bool) -> Result<(), Error> {
    if !matches!(value, Value::Map(_)) {
        return Ok(());
    }
    if !root && get(value, "$id").is_some() {
        return Err(unsupported(
            "converting a samplesheet schema with a nested `$id` reference scope",
        ));
    }
    for keyword in [
        "properties",
        "patternProperties",
        "$defs",
        "definitions",
        "dependentSchemas",
    ] {
        if let Some(children) = get(value, keyword) {
            for (_, child) in entries(&children) {
                reject_nested_ids(&child, false)?;
            }
        }
    }
    for keyword in [
        "items",
        "prefixItems",
        "additionalProperties",
        "additionalItems",
        "not",
        "if",
        "then",
        "else",
        "contains",
        "propertyNames",
        "unevaluatedItems",
        "unevaluatedProperties",
        "allOf",
        "anyOf",
        "oneOf",
    ] {
        if let Some(child) = get(value, keyword) {
            match child {
                Value::List(children) => {
                    for child in &children.borrow().items {
                        reject_nested_ids(child, false)?;
                    }
                }
                child => reject_nested_ids(&child, false)?,
            }
        }
    }
    Ok(())
}

fn unsupported(message: impl Into<String>) -> Error {
    Error::Unsupported {
        message: message.into(),
        span: Default::default(),
    }
}

/// Reads YAML and JSON structures, or CSV and TSV rows as maps, lists, or single values.
/// Schema item properties select maps; CSV and TSV scalars become numbers, booleans, or null.
fn file_object(
    file: &PathValue,
    text: &str,
    schema: &Value,
    converter: &Converter,
) -> Result<Value, Error> {
    let kind = file_type(file, text);
    let items = get(schema, "items")
        .map(|items| converter.resolve(&items))
        .transpose()?;
    if matches!(kind, "csv" | "tsv") {
        if get(schema, "type").is_some_and(|kind| kind.to_string() == "object") {
            return Err(thrown(format!(
                "CSV or TSV files are not supported. Use a JSON or YAML file instead of {}. \
                 (Expected a non-list data structure, which is not supported in CSV or TSV)",
                file.uri()
            )));
        }
        let properties = items
            .as_ref()
            .and_then(|items| get(items, "properties"))
            .or_else(|| get(schema, "properties"));
        let mut nested = false;
        for (_, property) in properties.iter().flat_map(entries) {
            let property = converter.resolve(&property)?;
            nested |= get(&property, "type").is_some_and(|kind| {
                let kind = kind.to_string();
                kind == "array" || kind == "object"
            });
        }
        if nested {
            return Err(thrown(format!(
                "Using \"type\": \"array\" or \"type\": \"object\" in schema with a \".{kind}\" \
                 samplesheet is not supported\n"
            )));
        }
    }
    let parsed = match kind {
        "yaml" => parse_yaml(text),
        "json" => parse_json(text),
        _ => {
            let header = items.is_some_and(|items| get(&items, "properties").is_some());
            let separator = if kind == "csv" { ',' } else { '\t' };
            return Ok(inferred(&csv(text, separator, header)));
        }
    };
    parsed.map_err(|exception| thrown(exception.message))
}

/// `csv`, `tsv`, `yaml` or `json` by the extension, else by the separator the first line
/// has more of.
fn file_type(file: &PathValue, text: &str) -> &'static str {
    let name = file.name();
    let extension = name.rsplit_once('.').map_or("", |(_, extension)| extension);
    match extension {
        "csv" => "csv",
        "tsv" => "tsv",
        "yml" | "yaml" => "yaml",
        "json" => "json",
        _ => {
            let first = text.lines().next().unwrap_or_default();
            if first.matches(',').count() > first.matches('\t').count() {
                "csv"
            } else {
                "tsv"
            }
        }
    }
}

/// The rows of a CSV or TSV text. With a header, each row is a map of the header fields,
/// with null for missing ones; separators at the end of the header are removed from every
/// line. Without, a row is its list of fields, or its only field.
fn csv(text: &str, separator: char, header: bool) -> Value {
    let mut lines: Vec<&str> = text.lines().collect();
    if header
        && lines
            .first()
            .is_some_and(|line| line.ends_with([',', '\t']))
    {
        lines = lines
            .into_iter()
            .take_while(|line| !line.is_empty())
            .map(|line| line.trim_end_matches([',', '\t']))
            .collect();
    }
    let mut rows = lines
        .into_iter()
        .filter(|line| !line.trim().is_empty())
        .map(|line| fields(line, separator));
    if !header {
        return Value::list(
            rows.map(|mut row| match row.len() {
                1 => Value::string(row.remove(0)),
                _ => Value::list(row.into_iter().map(Value::string).collect()),
            })
            .collect(),
        );
    }
    let names = rows.next().unwrap_or_default();
    Value::list(
        rows.map(|row| {
            Value::map(names.iter().enumerate().map(|(index, name)| {
                let field = row
                    .get(index)
                    .map_or(Value::Null, |f| Value::string(f.as_str()));
                (Value::string(name.as_str()), field)
            }))
        })
        .collect(),
    )
}

/// nf-schema's `inferType`: text as org.json reads it, so `1` is an integer, `2.5` a
/// decimal, `true` a boolean and empty text null; `007` stays text.
fn inferred(value: &Value) -> Value {
    match value {
        Value::Map(_) => Value::map(
            entries(value)
                .into_iter()
                .map(|(key, value)| (Value::string(key), inferred(&value))),
        ),
        Value::List(list) => Value::list(list.borrow().items.iter().map(inferred).collect()),
        Value::String(text) => text_value(text),
        _ => Value::Null,
    }
}

fn text_value(text: &str) -> Value {
    if text.is_empty() || text.eq_ignore_ascii_case("null") {
        return Value::Null;
    }
    if text.eq_ignore_ascii_case("true") {
        return Value::Boolean(true);
    }
    if text.eq_ignore_ascii_case("false") {
        return Value::Boolean(false);
    }
    let digits = text.strip_prefix('-').unwrap_or(text);
    if !digits.starts_with(|c: char| c.is_ascii_digit()) {
        return Value::string(text);
    }
    if text.contains(['.', 'e', 'E']) || text == "-0" {
        return match BigDecimal::parse(text) {
            // A negative zero is the double `-0.0`.
            Some(value) if text.starts_with('-') && value.signum() == 0 => {
                Value::Number(Number::Double(-0.0))
            }
            Some(value) => Value::Number(Number::BigDecimal(value)),
            None => Value::string(text),
        };
    }
    // A leading zero before another digit is not a number.
    let leading_zero = digits.len() > 1 && digits.starts_with('0');
    if leading_zero || !digits.chars().all(|c| c.is_ascii_digit()) {
        return Value::string(text);
    }
    if let Ok(value) = text.parse::<i32>() {
        return Value::Number(Number::Integer(value));
    }
    if let Ok(value) = text.parse::<i64>() {
        return Value::Number(Number::Long(value));
    }
    BigDecimal::parse(text).map_or_else(
        || Value::string(text),
        |value| Value::Number(Number::BigInteger(value.to_bigint())),
    )
}

/// JSON without nulls, as nf-schema writes the samplesheet for validation.
fn without_nulls(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Array(items) => items
            .into_iter()
            .filter(|item| !item.is_null())
            .map(without_nulls)
            .collect(),
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.into_iter()
                .filter(|(_, value)| !value.is_null())
                .map(|(key, value)| (key, without_nulls(value)))
                .collect(),
        ),
        other => other,
    }
}

/// Checks the samplesheet against the schema.
fn check(
    schema: &serde_json::Value,
    instance: &serde_json::Value,
    file: &PathValue,
    context: &mut Context,
) -> Result<(), Error> {
    let validator = super::paths::validator(schema, instance, context, false)
        .map_err(|error| thrown(format!("Invalid samplesheet schema: {error}")))?;
    let errors: Vec<String> = errors(&validator, instance, "field", super::lenient(context))
        .into_iter()
        .map(|(location, message)| {
            let mut parts = location.trim_start_matches('/').splitn(2, '/');
            let entry = parts.next().and_then(|index| index.parse::<usize>().ok());
            match (entry, parts.next()) {
                (Some(index), Some(field)) => {
                    let value = instance[index].get(field).map(plain).unwrap_or_default();
                    format!(
                        "-> Entry {}: Error for field '{field}' ({value}): {message}",
                        index + 1
                    )
                }
                (Some(index), None) => format!("-> Entry {}: {message}", index + 1),
                _ => format!("-> {message}"),
            }
        })
        .collect();
    if errors.is_empty() {
        return Ok(());
    }
    Err(thrown(format!(
        "The following errors have been detected in {}:\n\n{}\n",
        file.uri(),
        errors.join("\n")
    )))
}

/// The fields of one line; double quotes group text with separators and `""` is a quote.
fn fields(line: &str, separator: char) -> Vec<String> {
    let mut fields = vec![String::new()];
    let mut quoted = false;
    let mut characters = line.trim_end_matches('\r').chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '"' if quoted && characters.peek() == Some(&'"') => {
                characters.next();
                fields.last_mut().expect("a field").push('"');
            }
            '"' => quoted = !quoted,
            c if c == separator && !quoted => fields.push(String::new()),
            other => fields.last_mut().expect("a field").push(other),
        }
    }
    fields
        .into_iter()
        .map(|field| field.trim().to_owned())
        .collect()
}
